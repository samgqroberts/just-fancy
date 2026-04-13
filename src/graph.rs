use anyhow::{Context, Result};
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::{HashMap, HashSet};

use crate::justfile::JustDump;

/// A dependency graph for recipes
#[derive(Debug)]
pub struct RecipeGraph {
    /// The underlying directed graph
    graph: DiGraph<String, ()>,
    /// Map from recipe name to node index
    node_indices: HashMap<String, NodeIndex>,
}

impl RecipeGraph {
    /// Build a dependency graph from a parsed justfile
    ///
    /// Only includes the target recipe and its transitive dependencies.
    pub fn build(dump: &JustDump, target: &str) -> Result<Self> {
        let mut graph = DiGraph::new();
        let mut node_indices = HashMap::new();

        // First, collect all recipes we need (target + transitive deps)
        let needed_recipes = Self::collect_dependencies(dump, target)?;

        // Add nodes for all needed recipes
        for name in &needed_recipes {
            let idx = graph.add_node(name.clone());
            node_indices.insert(name.clone(), idx);
        }

        // Add edges for dependencies
        // Edge direction: dependency -> dependent (so toposort gives us execution order)
        for name in &needed_recipes {
            if let Some(recipe) = dump.get_recipe(name) {
                let dependent_idx = node_indices[name];
                for dep_name in recipe.dependency_names() {
                    if let Some(&dep_idx) = node_indices.get(dep_name) {
                        // Edge from dependency to dependent
                        graph.add_edge(dep_idx, dependent_idx, ());
                    }
                }
            }
        }

        let result = Self {
            graph,
            node_indices,
        };

        // Check for cycles
        result.check_cycles()?;

        Ok(result)
    }

    /// Collect all recipes needed to run the target (target + transitive deps)
    fn collect_dependencies(dump: &JustDump, target: &str) -> Result<HashSet<String>> {
        let mut needed = HashSet::new();
        let mut stack = vec![target.to_string()];

        while let Some(name) = stack.pop() {
            if needed.contains(&name) {
                continue;
            }

            let recipe = dump
                .get_recipe(&name)
                .with_context(|| format!("Recipe '{}' not found", name))?;

            needed.insert(name);

            for dep_name in recipe.dependency_names() {
                if !needed.contains(dep_name) {
                    stack.push(dep_name.to_string());
                }
            }
        }

        Ok(needed)
    }

    /// Check for circular dependencies
    fn check_cycles(&self) -> Result<()> {
        toposort(&self.graph, None).map_err(|cycle| {
            let node = &self.graph[cycle.node_id()];
            anyhow::anyhow!("Circular dependency detected involving recipe '{}'", node)
        })?;
        Ok(())
    }

    /// Get all recipes in a valid execution order (dependencies before dependents)
    pub fn execution_order(&self) -> Result<Vec<String>> {
        let sorted = toposort(&self.graph, None).map_err(|cycle| {
            let node = &self.graph[cycle.node_id()];
            anyhow::anyhow!("Circular dependency detected involving recipe '{}'", node)
        })?;

        Ok(sorted
            .into_iter()
            .map(|idx| self.graph[idx].clone())
            .collect())
    }

    /// Get the immediate dependencies of a recipe
    pub fn dependencies_of(&self, recipe: &str) -> Vec<String> {
        let Some(&idx) = self.node_indices.get(recipe) else {
            return vec![];
        };

        self.graph
            .neighbors_directed(idx, petgraph::Direction::Incoming)
            .map(|dep_idx| self.graph[dep_idx].clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::justfile::{Dependency, JustDump, Recipe};
    use std::collections::HashMap;

    fn make_recipe(name: &str, deps: &[&str]) -> Recipe {
        Recipe {
            name: name.to_string(),
            namepath: name.to_string(),
            dependencies: deps
                .iter()
                .map(|d| Dependency {
                    recipe: d.to_string(),
                    _arguments: vec![],
                })
                .collect(),
            private: false,
            parameters: vec![],
        }
    }

    fn make_dump(recipes: Vec<Recipe>, first: Option<&str>) -> JustDump {
        let mut map = HashMap::new();
        for r in recipes {
            map.insert(r.name.clone(), r);
        }
        JustDump {
            recipes: map,
            first: first.map(String::from),
            modules: HashMap::new(),
            source: None,
        }
    }

    #[test]
    fn test_simple_graph() {
        let dump = make_dump(
            vec![make_recipe("a", &[]), make_recipe("b", &["a"])],
            Some("b"),
        );

        let graph = RecipeGraph::build(&dump, "b").unwrap();
        let order = graph.execution_order().unwrap();
        assert_eq!(order.len(), 2);
        assert_eq!(order, vec!["a", "b"]);
    }

    #[test]
    fn test_diamond_dependency() {
        // Diamond: d depends on b and c, both depend on a
        let dump = make_dump(
            vec![
                make_recipe("a", &[]),
                make_recipe("b", &["a"]),
                make_recipe("c", &["a"]),
                make_recipe("d", &["b", "c"]),
            ],
            Some("d"),
        );

        let graph = RecipeGraph::build(&dump, "d").unwrap();
        let order = graph.execution_order().unwrap();
        assert_eq!(order.len(), 4);
        // a must come first, d must come last
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
    }

    #[test]
    fn test_dependencies_of() {
        let dump = make_dump(
            vec![
                make_recipe("a", &[]),
                make_recipe("b", &["a"]),
                make_recipe("c", &["a", "b"]),
            ],
            Some("c"),
        );

        let graph = RecipeGraph::build(&dump, "c").unwrap();

        assert_eq!(graph.dependencies_of("a"), Vec::<String>::new());
        assert_eq!(graph.dependencies_of("b"), vec!["a"]);

        let c_deps = graph.dependencies_of("c");
        assert!(c_deps.contains(&"a".to_string()));
        assert!(c_deps.contains(&"b".to_string()));
    }

    #[test]
    fn test_missing_recipe_error() {
        let dump = make_dump(vec![make_recipe("a", &[])], Some("a"));

        let result = RecipeGraph::build(&dump, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_single_recipe_no_deps() {
        let dump = make_dump(vec![make_recipe("solo", &[])], Some("solo"));

        let graph = RecipeGraph::build(&dump, "solo").unwrap();
        let order = graph.execution_order().unwrap();
        assert_eq!(order.len(), 1);
        assert_eq!(order, vec!["solo"]);
    }

    // --- Module support tests ---
    //
    // These tests exercise graph building in the presence of `just` modules.
    // They are marked #[ignore] because module support is not yet implemented.
    // They document the desired behavior and will be un-ignored once the
    // feature is complete.

    /// JSON shared by all module graph tests. See justfile.rs for the full
    /// description of this fixture.
    fn modules_dump() -> JustDump {
        // Use serde_json to construct the dump so we exercise the real
        // deserialization path and remain independent of struct field layout.
        let mut dump: JustDump = serde_json::from_str(
            r#"{
            "aliases": {},
            "assignments": {},
            "first": "all",
            "doc": null,
            "groups": [],
            "modules": {
                "build": {
                    "aliases": {}, "assignments": {}, "first": "compile",
                    "doc": null, "groups": [], "modules": {},
                    "recipes": {
                        "compile": {
                            "attributes": [], "body": [],
                            "dependencies": [{"arguments": [], "recipe": "generate"}],
                            "doc": null, "name": "compile", "namepath": "build::compile",
                            "parameters": [], "priors": 1,
                            "private": false, "quiet": false, "shebang": false
                        },
                        "generate": {
                            "attributes": [], "body": [],
                            "dependencies": [],
                            "doc": null, "name": "generate", "namepath": "build::generate",
                            "parameters": [], "priors": 0,
                            "private": false, "quiet": false, "shebang": false
                        }
                    },
                    "settings": {}, "source": "build.just",
                    "unexports": [], "warnings": []
                },
                "test": {
                    "aliases": {}, "assignments": {}, "first": "unit",
                    "doc": null, "groups": [], "modules": {},
                    "recipes": {
                        "unit": {
                            "attributes": [], "body": [],
                            "dependencies": [],
                            "doc": null, "name": "unit", "namepath": "test::unit",
                            "parameters": [], "priors": 0,
                            "private": false, "quiet": false, "shebang": false
                        },
                        "integration": {
                            "attributes": [], "body": [],
                            "dependencies": [],
                            "doc": null, "name": "integration", "namepath": "test::integration",
                            "parameters": [], "priors": 0,
                            "private": false, "quiet": false, "shebang": false
                        }
                    },
                    "settings": {}, "source": "test.just",
                    "unexports": [], "warnings": []
                }
            },
            "recipes": {
                "all": {
                    "attributes": [], "body": [],
                    "dependencies": [
                        {"arguments": [], "recipe": "compile"},
                        {"arguments": [], "recipe": "unit"}
                    ],
                    "doc": null, "name": "all", "namepath": "all",
                    "parameters": [], "priors": 2,
                    "private": false, "quiet": false, "shebang": false
                },
                "hello": {
                    "attributes": [], "body": [],
                    "dependencies": [],
                    "doc": null, "name": "hello", "namepath": "hello",
                    "parameters": [], "priors": 0,
                    "private": false, "quiet": false, "shebang": false
                }
            },
            "settings": {}, "source": "justfile",
            "unexports": [], "warnings": []
        }"#,
        )
        .unwrap();
        dump.flatten_modules();
        dump
    }

    /// Building a graph for the root `all` recipe must resolve its
    /// cross-module dependencies (`compile` -> `build::compile`,
    /// `unit` -> `test::unit`) and include their transitive deps.
    ///
    /// Expected execution order: generate, compile, unit (or unit, generate,
    /// compile — the two independent chains may interleave), then all.
    #[test]
    fn test_build_graph_for_root_recipe_with_cross_module_deps() {
        let dump = modules_dump();

        let graph = RecipeGraph::build(&dump, "all").unwrap();
        let order = graph.execution_order().unwrap();

        // all four recipes required: all, build::compile, build::generate, test::unit
        assert_eq!(order.len(), 4);

        // `all` must come last
        assert_eq!(order.last().unwrap(), "all");

        // `build::generate` must precede `build::compile`
        let pos_generate = order.iter().position(|n| n == "build::generate").unwrap();
        let pos_compile = order.iter().position(|n| n == "build::compile").unwrap();
        assert!(
            pos_generate < pos_compile,
            "build::generate must run before build::compile"
        );
    }

    /// Targeting a module recipe directly by its namepath (e.g.
    /// `build::compile`) must work.  The graph should include
    /// `build::compile` and its intra-module dependency `build::generate`.
    #[test]
    fn test_build_graph_targeting_module_recipe_directly() {
        let dump = modules_dump();

        let graph = RecipeGraph::build(&dump, "build::compile").unwrap();
        let order = graph.execution_order().unwrap();

        assert_eq!(order.len(), 2);
        assert_eq!(order[0], "build::generate");
        assert_eq!(order[1], "build::compile");
    }

    /// Targeting a leaf module recipe with no dependencies must work and
    /// produce a single-node graph.
    #[test]
    fn test_build_graph_targeting_leaf_module_recipe() {
        let dump = modules_dump();

        let graph = RecipeGraph::build(&dump, "test::unit").unwrap();
        let order = graph.execution_order().unwrap();

        assert_eq!(order.len(), 1);
        assert_eq!(order[0], "test::unit");
    }

    /// A root recipe that depends on multiple module recipes sharing the same
    /// short name (e.g. `check: core::check app::check sync::check`) must
    /// build a valid graph with no false cycle detection. This mirrors the
    /// real-world pattern in jott-app where root `check` fans out to every
    /// module's `check` recipe.
    #[test]
    fn test_build_graph_duplicate_cross_module_deps() {
        let mut dump: JustDump = serde_json::from_str(
            r#"{
            "first": "check",
            "modules": {
                "core": {
                    "first": "check", "modules": {},
                    "recipes": {
                        "test": {
                            "name": "test", "namepath": "core::test",
                            "dependencies": [], "parameters": [],
                            "private": false
                        },
                        "check": {
                            "name": "check", "namepath": "core::check",
                            "dependencies": [{"arguments": [], "recipe": "test"}],
                            "parameters": [], "private": false
                        }
                    }
                },
                "app": {
                    "first": "check", "modules": {},
                    "recipes": {
                        "lint": {
                            "name": "lint", "namepath": "app::lint",
                            "dependencies": [], "parameters": [],
                            "private": false
                        },
                        "check": {
                            "name": "check", "namepath": "app::check",
                            "dependencies": [{"arguments": [], "recipe": "lint"}],
                            "parameters": [], "private": false
                        }
                    }
                },
                "sync": {
                    "first": "check", "modules": {},
                    "recipes": {
                        "check": {
                            "name": "check", "namepath": "sync::check",
                            "dependencies": [], "parameters": [],
                            "private": false
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
        }"#,
        )
        .unwrap();
        dump.flatten_modules();

        let graph = RecipeGraph::build(&dump, "check").unwrap();
        let order = graph.execution_order().unwrap();

        // root check + 3 module checks + core::test + app::lint = 6
        assert_eq!(order.len(), 6, "expected 6 recipes, got: {:?}", order);

        // root check must come last
        assert_eq!(order.last().unwrap(), "check");

        // core::test must precede core::check
        let pos = |name: &str| order.iter().position(|n| n == name).unwrap();
        assert!(pos("core::test") < pos("core::check"));
        assert!(pos("app::lint") < pos("app::check"));

        // all three module checks must appear before root check
        assert!(pos("core::check") < pos("check"));
        assert!(pos("app::check") < pos("check"));
        assert!(pos("sync::check") < pos("check"));
    }

    /// Requesting a recipe that doesn't exist in any module must still return
    /// an error with "not found" in the message.
    ///
    /// This currently passes (the recipe is simply not found), and should
    /// continue to pass after module support is implemented.
    #[test]
    fn test_build_graph_nonexistent_module_recipe_errors() {
        let dump = modules_dump();

        let result = RecipeGraph::build(&dump, "build::nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
