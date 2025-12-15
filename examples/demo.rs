//! Demo of just-fancy
//!
//! This example creates a temporary justfile and runs just-fancy on it
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
use std::fs;
use std::io::Write;
use std::process::Command;

const DEMO_JUSTFILE: &str = r#"
# Demo justfile for just-fancy
# This demonstrates parallel execution and fancy output.

# ============================================================================
# Main entry points
# ============================================================================

# Run the full build and test pipeline
all: build lint test docs
    @echo "Pipeline complete!"

# Build everything (demonstrates parallel independent tasks)
build: compile-lib compile-bin generate-assets
    @echo "Build complete!"

# Run all tests (demonstrates tasks depending on build)
test: unit-tests integration-tests
    @echo "All tests passed!"

# ============================================================================
# Build tasks (no dependencies - can run in parallel)
# ============================================================================

# Compile the core library
compile-lib:
    #!/usr/bin/env bash
    echo "Compiling library..."
    sleep 0.8
    echo "  Building src/lib.rs"
    sleep 0.5
    echo "  Building src/utils.rs"
    sleep 0.4
    echo "  Building src/config.rs"
    sleep 0.3
    echo "Library compiled successfully"

# Compile the binary
compile-bin:
    #!/usr/bin/env bash
    echo "Compiling binary..."
    sleep 0.6
    echo "  Building src/main.rs"
    sleep 0.5
    echo "  Linking dependencies"
    sleep 0.4
    echo "Binary compiled successfully"

# Generate static assets
generate-assets:
    #!/usr/bin/env bash
    echo "Generating assets..."
    sleep 0.4
    echo "  Processing styles.css"
    sleep 0.3
    echo "  Bundling scripts.js"
    sleep 0.3
    echo "  Optimizing images"
    sleep 0.5
    echo "Assets generated successfully"

# ============================================================================
# Quality checks (depends on build)
# ============================================================================

# Run linter
lint: build
    #!/usr/bin/env bash
    echo "Running linter..."
    sleep 0.5
    echo "  Checking src/*.rs"
    sleep 0.6
    echo "  Checking tests/*.rs"
    sleep 0.4
    echo "Lint passed: 0 warnings"

# Generate documentation
docs: build
    #!/usr/bin/env bash
    echo "Generating documentation..."
    sleep 0.5
    echo "  Parsing doc comments"
    sleep 0.8
    echo "  Rendering HTML"
    sleep 0.5
    echo "Documentation generated: 42 pages"

# ============================================================================
# Test tasks (depend on build)
# ============================================================================

# Run unit tests
unit-tests: build
    #!/usr/bin/env bash
    echo "Running unit tests..."
    sleep 0.3
    echo "  test config::parse ... ok"
    sleep 0.2
    echo "  test utils::format ... ok"
    sleep 0.2
    echo "  test lib::process ... ok"
    sleep 0.3
    echo "  test lib::validate ... ok"
    sleep 0.2
    echo "Unit tests: 4 passed"

# Run integration tests
integration-tests: build
    #!/usr/bin/env bash
    echo "Running integration tests..."
    sleep 0.5
    echo "  Starting test server..."
    sleep 0.8
    echo "  test api::create ... ok"
    sleep 0.5
    echo "  test api::read ... ok"
    sleep 0.4
    echo "  test api::update ... ok"
    sleep 0.4
    echo "  test api::delete ... ok"
    sleep 0.3
    echo "  Stopping test server"
    sleep 0.2
    echo "Integration tests: 4 passed"

# ============================================================================
# Failure demonstration
# ============================================================================

# Demo: A task that fails
failing-task:
    #!/usr/bin/env bash
    echo "Starting risky operation..."
    sleep 0.5
    echo "Processing..."
    sleep 0.3
    echo "ERROR: Connection refused"
    exit 1

# Demo: A task that depends on the failing task
after-failure: failing-task
    @echo "This will never run"

# Demo: Run this to see failure + skip behavior
test-failure: after-failure
    @echo "This also won't run"

# ============================================================================
# Simple standalone tasks
# ============================================================================

# A quick standalone task
hello:
    @echo "Hello from just-fancy!"

# Clean build artifacts
clean:
    #!/usr/bin/env bash
    echo "Cleaning..."
    sleep 0.3
    echo "  Removing target/"
    sleep 0.2
    echo "  Removing *.log"
    sleep 0.1
    echo "Clean complete"
"#;

fn main() {
    // Create a temp directory for the demo
    let temp_dir = env::temp_dir().join("just-fancy-demo");
    fs::create_dir_all(&temp_dir).expect("Failed to create temp directory");

    // Write the demo justfile
    let justfile_path = temp_dir.join("justfile");
    let mut file = fs::File::create(&justfile_path).expect("Failed to create justfile");
    file.write_all(DEMO_JUSTFILE.as_bytes())
        .expect("Failed to write justfile");

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
