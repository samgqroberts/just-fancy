# just-fancy

A fancy wrapper for [just](https://github.com/casey/just) with parallel execution and pretty output.

## Features

- **Parallel execution** - Runs independent recipes concurrently, respecting dependencies
- **Pretty progress UI** - Spinners, status indicators, and live output previews
- **Log files** - Captures output to log files for failed tasks

## Installation

```sh
cargo install just-fancy
```

## Usage

```sh
# Run the default recipe
just-fancy

# Run a specific recipe
just-fancy build

# Run with arguments
just-fancy deploy production

# Limit parallel jobs
just-fancy -j 4 build

# List available recipes
just-fancy -l

# Disable fancy output (passthrough to just)
just-fancy --no-capture build
```

## Requirements

- [just](https://github.com/casey/just) must be installed and available in your PATH

## License

MIT
