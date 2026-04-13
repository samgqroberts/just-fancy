use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Represents the full output of `just --dump --dump-format=json`
#[derive(Debug, Deserialize)]
pub struct JustDump {
    /// Map of recipe name to recipe definition
    pub recipes: HashMap<String, Recipe>,
    /// The first/default recipe (if any)
    pub first: Option<String>,
    /// Nested modules (submodule justfiles)
    #[serde(default)]
    pub modules: HashMap<String, JustDump>,
    /// Path to the source file for this module
    #[serde(default)]
    pub source: Option<String>,
}

/// A recipe from the justfile
#[derive(Debug, Deserialize)]
pub struct Recipe {
    /// Recipe name
    pub name: String,
    /// Full namepath (e.g. `build::compile`); falls back to `name` for root recipes
    #[serde(default)]
    pub namepath: String,
    /// Dependencies that must run before this recipe
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// Whether this recipe is private (starts with _)
    #[serde(default)]
    pub private: bool,
    /// Parameters this recipe accepts
    #[serde(default)]
    pub parameters: Vec<Parameter>,
}

/// A dependency reference
#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    /// Name of the recipe this depends on
    pub recipe: String,
    /// Arguments to pass to the dependency (deserialized but not yet used)
    #[serde(default, rename = "arguments")]
    pub _arguments: Vec<serde_json::Value>,
}

/// A recipe parameter
#[derive(Debug, Deserialize)]
pub struct Parameter {
    /// Parameter name
    pub name: String,
    /// Default value (if any)
    pub default: Option<serde_json::Value>,
}

impl JustDump {
    /// Parse a justfile by running `just --dump --dump-format=json`
    pub fn parse(justfile_path: Option<&Path>) -> Result<Self> {
        let mut cmd = Command::new("just");
        cmd.arg("--dump").arg("--dump-format=json");

        if let Some(path) = justfile_path {
            cmd.arg("--justfile").arg(path);
        }

        let output = cmd
            .output()
            .context("Failed to run 'just'. Is it installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("just --dump failed: {}", stderr);
        }

        let json = String::from_utf8(output.stdout).context("Invalid UTF-8 in just output")?;

        let mut dump: JustDump =
            serde_json::from_str(&json).context("Failed to parse just --dump JSON output")?;
        dump.flatten_modules();
        Ok(dump)
    }

    /// Flatten all module recipes into `self.recipes` keyed by namepath,
    /// and resolve short dependency names to full namepaths.
    ///
    /// After this call, `self.modules` is cleared.
    pub fn flatten_modules(&mut self) {
        // Ensure root recipes have namepath set (fallback to name)
        for recipe in self.recipes.values_mut() {
            if recipe.namepath.is_empty() {
                recipe.namepath = recipe.name.clone();
            }
        }

        // Collect source paths BEFORE draining modules — needed for disambiguation later
        let mut source_paths: HashMap<String, String> = HashMap::new();
        if let Some(ref source) = self.source {
            source_paths.insert(String::new(), source.clone());
        }
        Self::collect_source_paths_recursive(&self.modules, "", &mut source_paths);

        // Recursively collect all module recipes
        let module_recipes = Self::collect_module_recipes(std::mem::take(&mut self.modules));
        for recipe in module_recipes {
            self.recipes.insert(recipe.namepath.clone(), recipe);
        }

        // Resolve short dependency names to full namepaths.
        //
        // Strategy: use source file as the primary resolution mechanism when
        // available, since the JSON dump loses module qualifiers. Fall back to
        // heuristic resolution (sibling/child module search) when source isn't
        // available (e.g., in tests with synthetic data).
        let all_keys: Vec<String> = self.recipes.keys().cloned().collect();

        for recipe in self.recipes.values_mut() {
            let prefix = if recipe.namepath.contains("::") {
                recipe.namepath.rsplitn(2, "::").nth(1).unwrap_or("")
            } else {
                ""
            };

            // Try source-based resolution first: read the justfile source and
            // match each JSON dep positionally to the source dep list.
            let source_path = source_paths.get(prefix);
            let source_deps =
                source_path.and_then(|p| Self::parse_deps_from_source(p, &recipe.name));

            if let Some(ref qualified_deps) = source_deps {
                if qualified_deps.len() == recipe.dependencies.len() {
                    // Positional match: JSON deps[i] corresponds to source deps[i]
                    for (dep, source_dep) in
                        recipe.dependencies.iter_mut().zip(qualified_deps.iter())
                    {
                        if source_dep.contains("::") {
                            // Qualified relative to the module, e.g. "wasm::build-debug"
                            dep.recipe = if prefix.is_empty() {
                                source_dep.clone()
                            } else {
                                format!("{}::{}", prefix, source_dep)
                            };
                        } else if !all_keys.contains(&dep.recipe) || dep.recipe == recipe.namepath
                        {
                            // Short name that needs resolution — try sibling/child heuristic
                            let resolved = Self::resolve_dep_heuristic(
                                &dep.recipe,
                                prefix,
                                &all_keys,
                            );
                            if let Some(namepath) = resolved {
                                dep.recipe = namepath;
                            }
                        }
                    }
                    continue;
                }
            }

            // Fallback: heuristic resolution without source file.
            // Group ALL deps by short name to handle duplicates (even non-consecutive).
            let mut dep_indices_by_name: HashMap<String, Vec<usize>> = HashMap::new();
            for (idx, dep) in recipe.dependencies.iter().enumerate() {
                dep_indices_by_name
                    .entry(dep.recipe.clone())
                    .or_default()
                    .push(idx);
            }

            for (dep_name, indices) in &dep_indices_by_name {
                // Skip if already resolved (exact match, not self-referential, no ambiguity)
                if indices.len() == 1 && all_keys.contains(dep_name) && *dep_name != recipe.namepath
                {
                    let suffix = format!("::{}", dep_name);
                    if !all_keys.iter().any(|k| k.ends_with(&suffix)) {
                        continue; // unambiguous exact match
                    }
                }

                let candidates = Self::find_candidates(dep_name, prefix, &all_keys);

                if candidates.len() == indices.len() {
                    // N deps → N distinct candidates
                    for (idx, candidate) in indices.iter().zip(candidates.iter()) {
                        recipe.dependencies[*idx].recipe = candidate.clone();
                    }
                } else if candidates.len() == 1 && indices.len() == 1 {
                    recipe.dependencies[indices[0]].recipe = candidates[0].clone();
                }
                // else: can't resolve — leave original (will error later with clear message)
            }
        }
    }

    /// Recursively collect source file paths from a modules map into a prefix→path mapping.
    /// The prefix for root is `""`, for module `app` it is `"app"`, etc.
    fn collect_source_paths_recursive(
        modules: &HashMap<String, JustDump>,
        prefix: &str,
        paths: &mut HashMap<String, String>,
    ) {
        for (mod_name, mod_data) in modules {
            let mod_prefix = if prefix.is_empty() {
                mod_name.clone()
            } else {
                format!("{}::{}", prefix, mod_name)
            };
            if let Some(ref source) = mod_data.source {
                paths.insert(mod_prefix.clone(), source.clone());
            }
            Self::collect_source_paths_recursive(&mod_data.modules, &mod_prefix, paths);
        }
    }

    /// Parse a justfile source to extract the qualified dependency names for a given recipe.
    /// Returns None if the source can't be read or the recipe header can't be found.
    fn parse_deps_from_source(source_path: &str, recipe_name: &str) -> Option<Vec<String>> {
        let content = std::fs::read_to_string(source_path).ok()?;
        for line in content.lines() {
            let trimmed = line.trim();
            // Skip comments, attributes, empty lines, and indented body lines
            if trimmed.starts_with('#')
                || trimmed.starts_with('[')
                || trimmed.is_empty()
                || line.starts_with(' ')
                || line.starts_with('\t')
            {
                continue;
            }
            // Recipe header format: recipe-name [params...]: [deps...]
            if let Some(colon_pos) = trimmed.find(':') {
                let before_colon = &trimmed[..colon_pos];
                let first_word = before_colon.split_whitespace().next().unwrap_or("");
                if first_word == recipe_name {
                    let after_colon = trimmed[colon_pos + 1..].trim();
                    if after_colon.is_empty() {
                        return Some(vec![]);
                    }
                    // Split deps by whitespace; stop at `&&` (post-deps separator)
                    let deps: Vec<String> = after_colon
                        .split_whitespace()
                        .take_while(|s| *s != "&&")
                        .map(|s| s.to_string())
                        .collect();
                    return Some(deps);
                }
            }
        }
        None
    }

    /// Find candidate namepaths for a short dep name within a module scope.
    /// Checks siblings first, then direct child modules.
    fn find_candidates(dep_name: &str, prefix: &str, all_keys: &[String]) -> Vec<String> {
        let suffix = format!("::{}", dep_name);
        if !prefix.is_empty() {
            // Module recipe — try sibling first
            let sibling = format!("{}{}", prefix, suffix);
            if all_keys.contains(&sibling) {
                return vec![sibling];
            }
            // Search direct child modules: {prefix}::*::{dep_name}
            all_keys
                .iter()
                .filter(|np| {
                    np.ends_with(&suffix)
                        && np.starts_with(&format!("{}::", prefix))
                        && np.matches("::").count() == prefix.matches("::").count() + 2
                })
                .cloned()
                .collect()
        } else {
            // Root recipe — find direct child module recipes matching *::{dep_name}
            all_keys
                .iter()
                .filter(|np| np.ends_with(&suffix) && np.matches("::").count() == 1)
                .cloned()
                .collect()
        }
    }

    /// Resolve a single dep name using sibling/child heuristic. Returns the
    /// resolved namepath if exactly one candidate is found.
    fn resolve_dep_heuristic(
        dep_name: &str,
        prefix: &str,
        all_keys: &[String],
    ) -> Option<String> {
        let candidates = Self::find_candidates(dep_name, prefix, all_keys);
        if candidates.len() == 1 {
            Some(candidates.into_iter().next().unwrap())
        } else {
            None
        }
    }

    /// Recursively collect all recipes from a modules map, returning them as a flat list.
    fn collect_module_recipes(modules: HashMap<String, JustDump>) -> Vec<Recipe> {
        let mut result = Vec::new();
        for (_, mut module) in modules {
            // Ensure module recipes have namepath set
            for recipe in module.recipes.values_mut() {
                if recipe.namepath.is_empty() {
                    recipe.namepath = recipe.name.clone();
                }
            }
            // Recurse into nested modules first
            let nested = Self::collect_module_recipes(std::mem::take(&mut module.modules));
            result.extend(nested);
            result.extend(module.recipes.into_values());
        }
        result
    }

    /// Get the default recipe to run
    pub fn default_recipe(&self) -> Option<&str> {
        self.first.as_deref()
    }

    /// Get a recipe by name
    pub fn get_recipe(&self, name: &str) -> Option<&Recipe> {
        self.recipes.get(name)
    }

    /// List all public recipes
    pub fn public_recipes(&self) -> impl Iterator<Item = &Recipe> {
        self.recipes.values().filter(|r| !r.private)
    }
}

impl Recipe {
    /// Get the names of recipes this recipe depends on
    pub fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.dependencies.iter().map(|d| d.recipe.as_str())
    }

    /// Check if this recipe has required parameters (no default value)
    #[cfg(test)]
    pub fn has_required_params(&self) -> bool {
        self.parameters.iter().any(|p| p.default.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JSON dump from `just --dump --dump-format=json` for a justfile that uses modules.
    ///
    /// Root has `all` (depends on `compile` and `unit` by short name) and `hello`.
    /// Module `build` has `compile` (depends on `generate`) and `generate`.
    /// Module `test` has `unit` and `integration`.
    const MODULES_JSON: &str = r#"{
        "aliases": {},
        "assignments": {},
        "first": "all",
        "doc": null,
        "groups": [],
        "modules": {
            "build": {
                "aliases": {},
                "assignments": {},
                "first": "compile",
                "doc": null,
                "groups": [],
                "modules": {},
                "recipes": {
                    "compile": {
                        "attributes": [],
                        "body": [["@echo \"Compiling...\""]],
                        "dependencies": [{"arguments": [], "recipe": "generate"}],
                        "doc": "Compile the project",
                        "name": "compile",
                        "namepath": "build::compile",
                        "parameters": [],
                        "priors": 1,
                        "private": false,
                        "quiet": false,
                        "shebang": false
                    },
                    "generate": {
                        "attributes": [],
                        "body": [["@echo \"Generating...\""]],
                        "dependencies": [],
                        "doc": "Generate code",
                        "name": "generate",
                        "namepath": "build::generate",
                        "parameters": [],
                        "priors": 0,
                        "private": false,
                        "quiet": false,
                        "shebang": false
                    }
                },
                "settings": {},
                "source": "build.just",
                "unexports": [],
                "warnings": []
            },
            "test": {
                "aliases": {},
                "assignments": {},
                "first": "unit",
                "doc": null,
                "groups": [],
                "modules": {},
                "recipes": {
                    "unit": {
                        "attributes": [],
                        "body": [["@echo \"Running unit tests...\""]],
                        "dependencies": [],
                        "doc": "Run unit tests",
                        "name": "unit",
                        "namepath": "test::unit",
                        "parameters": [],
                        "priors": 0,
                        "private": false,
                        "quiet": false,
                        "shebang": false
                    },
                    "integration": {
                        "attributes": [],
                        "body": [["@echo \"Running integration tests...\""]],
                        "dependencies": [],
                        "doc": "Run integration tests",
                        "name": "integration",
                        "namepath": "test::integration",
                        "parameters": [],
                        "priors": 0,
                        "private": false,
                        "quiet": false,
                        "shebang": false
                    }
                },
                "settings": {},
                "source": "test.just",
                "unexports": [],
                "warnings": []
            }
        },
        "recipes": {
            "all": {
                "attributes": [],
                "body": [["@echo \"All done!\""]],
                "dependencies": [
                    {"arguments": [], "recipe": "compile"},
                    {"arguments": [], "recipe": "unit"}
                ],
                "doc": "Run everything",
                "name": "all",
                "namepath": "all",
                "parameters": [],
                "priors": 2,
                "private": false,
                "quiet": false,
                "shebang": false
            },
            "hello": {
                "attributes": [],
                "body": [["@echo \"Hello from root!\""]],
                "dependencies": [],
                "doc": "A root-level recipe",
                "name": "hello",
                "namepath": "hello",
                "parameters": [],
                "priors": 0,
                "private": false,
                "quiet": false,
                "shebang": false
            }
        },
        "settings": {},
        "source": "justfile",
        "unexports": [],
        "warnings": []
    }"#;

    #[test]
    fn test_parse_json() {
        // Test parsing just's JSON format directly
        let json = r#"{
            "recipes": {
                "build": {
                    "name": "build",
                    "dependencies": [],
                    "private": false,
                    "parameters": []
                },
                "test": {
                    "name": "test",
                    "dependencies": [{"recipe": "build", "arguments": []}],
                    "private": false,
                    "parameters": []
                },
                "_private": {
                    "name": "_private",
                    "dependencies": [],
                    "private": true,
                    "parameters": []
                }
            },
            "first": "build"
        }"#;

        let dump: JustDump = serde_json::from_str(json).unwrap();

        assert_eq!(dump.default_recipe(), Some("build"));
        assert_eq!(dump.recipes.len(), 3);

        let build = dump.get_recipe("build").unwrap();
        assert_eq!(build.name, "build");
        assert!(build.dependencies.is_empty());

        let test = dump.get_recipe("test").unwrap();
        assert_eq!(test.dependency_names().collect::<Vec<_>>(), vec!["build"]);
    }

    #[test]
    fn test_public_recipes() {
        let json = r#"{
            "recipes": {
                "public1": {"name": "public1", "dependencies": [], "private": false, "parameters": []},
                "public2": {"name": "public2", "dependencies": [], "private": false, "parameters": []},
                "_private": {"name": "_private", "dependencies": [], "private": true, "parameters": []}
            },
            "first": "public1"
        }"#;

        let dump: JustDump = serde_json::from_str(json).unwrap();
        let public: Vec<_> = dump.public_recipes().collect();

        assert_eq!(public.len(), 2);
        assert!(public.iter().all(|r| !r.private));
    }

    #[test]
    fn test_recipe_with_parameters() {
        let json = r#"{
            "recipes": {
                "deploy": {
                    "name": "deploy",
                    "dependencies": [],
                    "private": false,
                    "parameters": [
                        {"name": "env", "default": null},
                        {"name": "verbose", "default": "false"}
                    ]
                }
            },
            "first": "deploy"
        }"#;

        let dump: JustDump = serde_json::from_str(json).unwrap();
        let deploy = dump.get_recipe("deploy").unwrap();

        assert_eq!(deploy.parameters.len(), 2);
        assert!(deploy.has_required_params()); // "env" has no default
    }

    #[test]
    fn test_parse_justfile_integration() {
        // This test requires `just` to be installed
        if Command::new("just").arg("--version").output().is_err() {
            return;
        }

        // Try to parse the test.justfile in the repo
        let result = JustDump::parse(Some(Path::new("test.justfile")));
        if let Ok(dump) = result {
            // Should have recipes
            assert!(!dump.recipes.is_empty());
        }
    }

    // --- Module support tests ---
    //
    // The tests below document the current broken behavior and the desired API
    // for module support. Tests that exercise behavior not yet implemented are
    // marked #[ignore] so `cargo test` continues to pass.

    /// Parsing a JSON dump with modules via raw `serde_json::from_str` (without calling
    /// `flatten_modules`) deserializes successfully but module recipes remain inaccessible.
    #[test]
    fn test_parse_json_with_modules_doesnt_error() {
        let dump: JustDump = serde_json::from_str(MODULES_JSON).unwrap();

        // Root-level recipes are present
        assert_eq!(dump.recipes.len(), 2);
        assert!(dump.get_recipe("all").is_some());
        assert!(dump.get_recipe("hello").is_some());

        // Without `flatten_modules()`, module recipes are not in the root recipes map.
        // This tests the raw deserialization path.
        assert!(dump.get_recipe("build::compile").is_none());
        assert!(dump.get_recipe("test::unit").is_none());
    }

    /// After module support is implemented, `get_recipe` (or a dedicated
    /// `get_recipe_by_namepath`) should resolve a namepath like
    /// `"build::compile"` to the recipe inside the `build` module.
    #[test]
    fn test_get_recipe_by_namepath_resolves_module_recipe() {
        let mut dump: JustDump = serde_json::from_str(MODULES_JSON).unwrap();
        dump.flatten_modules();

        let compile = dump.get_recipe("build::compile");
        assert!(
            compile.is_some(),
            "get_recipe(\"build::compile\") should resolve the recipe inside the build module"
        );
        let compile = compile.unwrap();
        assert_eq!(compile.name, "compile");

        let generate = dump.get_recipe("build::generate");
        assert!(generate.is_some());

        let unit = dump.get_recipe("test::unit");
        assert!(unit.is_some());

        let integration = dump.get_recipe("test::integration");
        assert!(integration.is_some());
    }

    /// `public_recipes()` should include recipes from all modules, not just
    /// the root.
    #[test]
    fn test_public_recipes_includes_module_recipes() {
        let mut dump: JustDump = serde_json::from_str(MODULES_JSON).unwrap();
        dump.flatten_modules();

        // Root has 2 public recipes; modules add 4 more (compile, generate,
        // unit, integration) for a total of 6.
        let public: Vec<_> = dump.public_recipes().collect();
        assert_eq!(
            public.len(),
            6,
            "public_recipes() should include module recipes; got {:?}",
            public.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    /// `flatten_modules` handles modules nested 2+ levels deep.
    ///
    /// Verifies that recipes from `infra::deploy` (a sub-module of `infra`) are
    /// accessible by namepath, and that dependency resolution works correctly at
    /// each level of nesting.
    #[test]
    fn test_nested_modules_two_levels_deep() {
        let json = r#"{
            "first": "ship",
            "modules": {
                "infra": {
                    "first": "provision",
                    "modules": {
                        "deploy": {
                            "first": "setup",
                            "modules": {},
                            "recipes": {
                                "setup": {
                                    "name": "setup", "namepath": "infra::deploy::setup",
                                    "dependencies": [], "parameters": [],
                                    "private": false
                                },
                                "run": {
                                    "name": "run", "namepath": "infra::deploy::run",
                                    "dependencies": [{"recipe": "setup", "arguments": []}],
                                    "parameters": [], "private": false
                                }
                            }
                        }
                    },
                    "recipes": {
                        "provision": {
                            "name": "provision", "namepath": "infra::provision",
                            "dependencies": [], "parameters": [],
                            "private": false
                        }
                    }
                }
            },
            "recipes": {
                "ship": {
                    "name": "ship", "namepath": "ship",
                    "dependencies": [{"recipe": "provision", "arguments": []}],
                    "parameters": [], "private": false
                }
            }
        }"#;

        let mut dump: JustDump = serde_json::from_str(json).unwrap();
        dump.flatten_modules();

        // All 4 recipes should be accessible by namepath
        assert_eq!(dump.recipes.len(), 4);
        assert!(dump.get_recipe("ship").is_some());
        assert!(dump.get_recipe("infra::provision").is_some());
        assert!(dump.get_recipe("infra::deploy::setup").is_some());
        assert!(dump.get_recipe("infra::deploy::run").is_some());

        // infra::deploy::run's dep "setup" should resolve to "infra::deploy::setup"
        let run = dump.get_recipe("infra::deploy::run").unwrap();
        assert_eq!(
            run.dependencies[0].recipe,
            "infra::deploy::setup",
            "infra::deploy::run dep should resolve from \"setup\" to \"infra::deploy::setup\""
        );

        // root ship's dep "provision" should resolve to "infra::provision"
        let ship = dump.get_recipe("ship").unwrap();
        assert_eq!(
            ship.dependencies[0].recipe,
            "infra::provision",
            "ship dep should resolve from \"provision\" to \"infra::provision\""
        );
    }

    /// When a root recipe depends on multiple module recipes that share the same short name,
    /// all N duplicate deps must resolve to N distinct module namepaths — not all to the same one.
    #[test]
    fn test_flatten_modules_duplicate_cross_module_deps() {
        let json = r#"{
            "first": "check",
            "modules": {
                "core": {
                    "first": "check", "modules": {},
                    "recipes": {
                        "check": {
                            "name": "check", "namepath": "core::check",
                            "dependencies": [], "parameters": [], "private": false
                        }
                    }
                },
                "app": {
                    "first": "check", "modules": {},
                    "recipes": {
                        "check": {
                            "name": "check", "namepath": "app::check",
                            "dependencies": [], "parameters": [], "private": false
                        }
                    }
                },
                "sync": {
                    "first": "check", "modules": {},
                    "recipes": {
                        "check": {
                            "name": "check", "namepath": "sync::check",
                            "dependencies": [], "parameters": [], "private": false
                        }
                    }
                }
            },
            "recipes": {
                "check": {
                    "name": "check", "namepath": "check",
                    "dependencies": [
                        {"recipe": "check", "arguments": []},
                        {"recipe": "check", "arguments": []},
                        {"recipe": "check", "arguments": []}
                    ],
                    "parameters": [], "private": false
                }
            }
        }"#;

        let mut dump: JustDump = serde_json::from_str(json).unwrap();
        dump.flatten_modules();

        // 3 module recipes + 1 root = 4 total
        assert_eq!(dump.recipes.len(), 4);

        let check = dump.get_recipe("check").unwrap();
        assert_eq!(check.dependencies.len(), 3, "root check should have 3 resolved deps");

        // All 3 resolved deps should be distinct module namepaths, not self-referential
        let mut dep_names: Vec<String> = check
            .dependencies
            .iter()
            .map(|d| d.recipe.clone())
            .collect();
        dep_names.sort();

        assert_eq!(
            dep_names,
            vec!["app::check", "core::check", "sync::check"],
            "root check's deps should resolve to the 3 distinct module namepaths"
        );

        // No dep should point back to the root "check" (self-reference)
        assert!(
            !dep_names.contains(&"check".to_string()),
            "no dep should be self-referential"
        );
    }

    /// When a module recipe has a single dep that matches multiple child module recipes by short
    /// name, the source file is used to disambiguate using the qualified name written there.
    ///
    /// This covers the case where `app/mod.just` contains:
    ///   android-debug-build: wasm::build-debug
    /// The JSON loses the `wasm::` prefix, so after flattening the dep is just `"build-debug"`.
    /// Both `app::wasm::build-debug` and `app::browser-engine::build-debug` exist, so candidates
    /// has 2 entries for count=1. The source file is used to recover `wasm::build-debug` and
    /// resolve to `app::wasm::build-debug`.
    #[test]
    fn test_source_file_disambiguation_for_ambiguous_child_module_dep() {
        use std::io::Write;

        // Write a temporary source file simulating app/mod.just
        let mut source_file = tempfile::NamedTempFile::new().unwrap();
        writeln!(source_file, "android-debug-build: wasm::build-debug").unwrap();
        let source_path = source_file.path().to_str().unwrap().to_string();

        let json = format!(
            r#"{{
            "first": "android-debug-build",
            "modules": {{
                "app": {{
                    "first": "android-debug-build",
                    "source": {source_path_json},
                    "modules": {{
                        "wasm": {{
                            "first": "build-debug", "modules": {{}},
                            "recipes": {{
                                "build-debug": {{
                                    "name": "build-debug",
                                    "namepath": "app::wasm::build-debug",
                                    "dependencies": [], "parameters": [], "private": false
                                }}
                            }}
                        }},
                        "browser-engine": {{
                            "first": "build-debug", "modules": {{}},
                            "recipes": {{
                                "build-debug": {{
                                    "name": "build-debug",
                                    "namepath": "app::browser-engine::build-debug",
                                    "dependencies": [], "parameters": [], "private": false
                                }}
                            }}
                        }}
                    }},
                    "recipes": {{
                        "android-debug-build": {{
                            "name": "android-debug-build",
                            "namepath": "app::android-debug-build",
                            "dependencies": [{{"recipe": "build-debug", "arguments": []}}],
                            "parameters": [], "private": false
                        }}
                    }}
                }}
            }},
            "recipes": {{}}
        }}"#,
            source_path_json = serde_json::to_string(&source_path).unwrap(),
        );

        let mut dump: JustDump = serde_json::from_str(&json).unwrap();
        dump.flatten_modules();

        let recipe = dump.get_recipe("app::android-debug-build").unwrap();
        assert_eq!(recipe.dependencies.len(), 1);
        assert_eq!(
            recipe.dependencies[0].recipe,
            "app::wasm::build-debug",
            "dep should resolve to app::wasm::build-debug via source file disambiguation"
        );
    }

    /// Non-consecutive duplicate deps interleaved with other dep names must all
    /// resolve correctly via source file positional matching. This mirrors the
    /// jott-app pattern where root `check` depends on `browser-engine::check`,
    /// `core::check`, ..., `app-check`, ..., `tauri::check` — the JSON loses
    /// module qualifiers and produces `["check", "check", ..., "app-check", ..., "check"]`.
    #[test]
    fn test_source_resolution_non_consecutive_duplicate_deps() {
        use std::io::Write;

        let mut source_file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            source_file,
            "check: core::check app-check sync::check"
        )
        .unwrap();
        let source_path = source_file.path().to_str().unwrap().to_string();

        let json = format!(
            r#"{{
            "first": "check",
            "source": {source_path_json},
            "modules": {{
                "core": {{
                    "first": "check", "modules": {{}},
                    "recipes": {{
                        "check": {{
                            "name": "check", "namepath": "core::check",
                            "dependencies": [], "parameters": [], "private": false
                        }}
                    }}
                }},
                "sync": {{
                    "first": "check", "modules": {{}},
                    "recipes": {{
                        "check": {{
                            "name": "check", "namepath": "sync::check",
                            "dependencies": [], "parameters": [], "private": false
                        }}
                    }}
                }}
            }},
            "recipes": {{
                "check": {{
                    "name": "check", "namepath": "check",
                    "dependencies": [
                        {{"recipe": "check", "arguments": []}},
                        {{"recipe": "app-check", "arguments": []}},
                        {{"recipe": "check", "arguments": []}}
                    ],
                    "parameters": [], "private": false
                }},
                "app-check": {{
                    "name": "app-check", "namepath": "app-check",
                    "dependencies": [], "parameters": [], "private": false
                }}
            }}
        }}"#,
            source_path_json = serde_json::to_string(&source_path).unwrap(),
        );

        let mut dump: JustDump = serde_json::from_str(&json).unwrap();
        dump.flatten_modules();

        let check = dump.get_recipe("check").unwrap();
        let dep_names: Vec<&str> = check.dependencies.iter().map(|d| d.recipe.as_str()).collect();

        assert_eq!(
            dep_names,
            vec!["core::check", "app-check", "sync::check"],
            "non-consecutive duplicate 'check' deps should resolve positionally from source"
        );
    }

    /// `parse_deps_from_source` correctly extracts deps from recipe headers.
    #[test]
    fn test_parse_deps_from_source() {
        use std::io::Write;

        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"# a comment
[attribute]
recipe-with-params arg1 arg2: dep-a wasm::dep-b
no-deps:
    echo body line
other: dep-c && dep-d
"#
        )
        .unwrap();
        let path = f.path().to_str().unwrap();

        let deps = JustDump::parse_deps_from_source(path, "recipe-with-params").unwrap();
        assert_eq!(deps, vec!["dep-a", "wasm::dep-b"]);

        let deps = JustDump::parse_deps_from_source(path, "no-deps").unwrap();
        assert!(deps.is_empty());

        // `&&` marks post-deps; only pre-deps are returned
        let deps = JustDump::parse_deps_from_source(path, "other").unwrap();
        assert_eq!(deps, vec!["dep-c"]);

        // Non-existent recipe returns None
        assert!(JustDump::parse_deps_from_source(path, "missing").is_none());
    }
}
