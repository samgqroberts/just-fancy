//! Demo of just-fancy
//!
//! This example runs just-fancy on the included justfile
//! to demonstrate parallel execution with fancy output.
//!
//! Run with: cargo run --example demo
//!
//! Or try specific scenarios:
//!   cargo run --example demo -- all           # Full pipeline
//!   cargo run --example demo -- build         # Just build step
//!   cargo run --example demo -- test-failure  # Demo failure handling
//!   cargo run --example demo -- --list        # List recipes

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Get the path to the justfile in the same directory as this example
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let justfile_path = PathBuf::from(manifest_dir)
        .join("examples")
        .join("demo")
        .join("justfile");

    // Get the path to just-fancy binary (in target directory)
    let exe_path = env::current_exe().expect("Failed to get current exe path");
    let target_dir = exe_path
        .parent() // examples
        .and_then(|p| p.parent()) // target/{debug,release}
        .expect("Failed to find target directory");
    let just_fancy = target_dir.join("just-fancy");

    // Check if just-fancy is built
    if !just_fancy.exists() {
        eprintln!("Error: just-fancy binary not found at {:?}", just_fancy);
        eprintln!("Run 'cargo build' first to build the main binary.");
        std::process::exit(1);
    }

    // Collect args (skip the example binary name)
    let args: Vec<String> = env::args().skip(1).collect();

    // Default to "all" if no args provided
    let args = if args.is_empty() {
        vec!["all".to_string()]
    } else {
        args
    };

    println!("Running just-fancy demo...");
    println!("Justfile: {}", justfile_path.display());
    println!(
        "Command: just-fancy -f {} {}",
        justfile_path.display(),
        args.join(" ")
    );
    println!();

    // Run just-fancy
    let status = Command::new(&just_fancy)
        .arg("-f")
        .arg(&justfile_path)
        .args(&args)
        .status()
        .expect("Failed to run just-fancy");

    std::process::exit(status.code().unwrap_or(1));
}
