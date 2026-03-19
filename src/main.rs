mod executor;
mod graph;
mod justfile;
mod output;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use std::io::IsTerminal;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::executor::Executor;
use crate::graph::RecipeGraph;
use crate::justfile::JustDump;
use crate::ui::AnyUI;

/// A fancy wrapper for just with parallel execution and pretty output
#[derive(Parser, Debug)]
#[command(name = "just-fancy")]
#[command(version, about, long_about = None)]
struct Args {
    /// Recipe to run (default: first recipe in justfile)
    #[arg(value_name = "RECIPE")]
    recipe: Option<String>,

    /// Arguments to pass to the recipe
    #[arg(value_name = "ARGUMENTS", trailing_var_arg = true)]
    arguments: Vec<String>,

    /// Path to justfile
    #[arg(short = 'f', long)]
    justfile: Option<PathBuf>,

    /// Maximum number of parallel jobs (default: number of CPUs)
    #[arg(short = 'j', long)]
    jobs: Option<usize>,

    /// List available recipes
    #[arg(short = 'l', long)]
    list: bool,

    /// Run just directly without parallel execution or output capture (passthrough mode)
    #[arg(long)]
    no_capture: bool,

    /// Disable fancy output, use plain line-based status (auto-enabled when stdout is not a TTY)
    #[arg(long)]
    plain: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Parse the justfile
    let dump = JustDump::parse(args.justfile.as_deref()).context("Failed to parse justfile")?;

    // Handle --list
    if args.list {
        println!("Available recipes:");
        let mut recipes: Vec<_> = dump.public_recipes().collect();
        recipes.sort_by_key(|r| &r.name);
        for recipe in recipes {
            if recipe.parameters.is_empty() {
                println!("    {}", recipe.name);
            } else {
                let params: Vec<_> = recipe
                    .parameters
                    .iter()
                    .map(|p| {
                        if p.default.is_some() {
                            format!("[{}]", p.name)
                        } else {
                            p.name.clone()
                        }
                    })
                    .collect();
                println!("    {} {}", recipe.name, params.join(" "));
            }
        }
        return Ok(());
    }

    // Determine target recipe
    let target = args
        .recipe
        .or_else(|| dump.default_recipe().map(String::from))
        .context("No recipe specified and no default recipe found")?;

    // Verify recipe exists
    dump.get_recipe(&target)
        .with_context(|| format!("Recipe '{}' not found", target))?;

    // Build dependency graph
    let graph = RecipeGraph::build(&dump, &target).context("Failed to build dependency graph")?;

    // Handle --no-capture mode
    if args.no_capture {
        return run_no_capture(&target, args.justfile.as_deref(), &args.arguments).await;
    }

    // Get execution order for UI setup
    let recipes = graph.execution_order()?;

    // Use plain output when stdout is not a TTY or when explicitly requested
    let is_plain = args.plain || !std::io::stdout().is_terminal();

    // Create UI
    let ui = if is_plain {
        AnyUI::Plain(crate::ui::PlainUI::new(&recipes))
    } else {
        AnyUI::Fancy(crate::ui::UI::new(&recipes))
    };

    // Create executor
    let max_jobs = args.jobs.unwrap_or_else(num_cpus::get);
    let executor = Executor::new(max_jobs)
        .justfile(args.justfile.map(|p| p.to_string_lossy().to_string()))
        .args(args.arguments);

    // Create channel for events
    let (event_tx, event_rx) = mpsc::channel(100);

    // Run executor and UI concurrently
    let executor_handle = {
        let target = target.clone();
        tokio::spawn(async move { executor.execute(&graph, &target, event_tx).await })
    };

    let (_, failed, _) = ui.run(event_rx).await;

    // Wait for executor to finish
    let _success_result = executor_handle.await??;

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Run in no-capture mode (just pass through to just)
async fn run_no_capture(
    recipe: &str,
    justfile: Option<&std::path::Path>,
    args: &[String],
) -> Result<()> {
    use std::process::Command;

    let mut cmd = Command::new("just");

    if let Some(path) = justfile {
        cmd.arg("--justfile").arg(path);
    }

    cmd.arg(recipe);
    for arg in args {
        cmd.arg(arg);
    }

    let status = cmd.status().context("Failed to run just")?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
