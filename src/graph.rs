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
}
