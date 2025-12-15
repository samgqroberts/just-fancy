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
}

/// A recipe from the justfile
#[derive(Debug, Deserialize)]
pub struct Recipe {
    /// Recipe name
    pub name: String,
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
#[derive(Debug, Deserialize)]
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

        serde_json::from_str(&json).context("Failed to parse just --dump JSON output")
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
}
