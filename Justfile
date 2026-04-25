# Thebe — task runner (https://github.com/casey/just)

# Build the `thebe` CLI in debug mode
build:
    cargo build -p thebe-cli

# Build the `thebe` CLI in release mode
build-release:
    cargo build -p thebe-cli --release

# Run all workspace tests
test:
    cargo test

# Run clippy across the workspace
lint:
    cargo clippy --all-targets -- -D warnings

# Format the workspace
fmt:
    cargo fmt

# Install local dependencies for the perf runner
perf-install:
    npm install --prefix scripts/perf

# List all configured perf targets
perf-list:
    node scripts/perf/run.cjs --list-targets

# Run the local perf harness against the counter-app example
perf:
    node scripts/perf/run.cjs

# Run the local perf harness against a specific configured target
perf-target target:
    node scripts/perf/run.cjs --target {{target}}

# Run the local perf harness without rebuilding the example first
perf-quick:
    node scripts/perf/run.cjs --skip-build

# Run the local perf harness against a specific target without rebuilding it first
perf-target-quick target:
    node scripts/perf/run.cjs --target {{target}} --skip-build

# Save the current run as a named local baseline under benchmarks/results/baselines/
perf-baseline name:
    node scripts/perf/run.cjs --save-baseline {{name}}

# Compare the current run against a named local baseline
perf-compare name:
    node scripts/perf/run.cjs --compare-to {{name}}

# Compare a specific target against a named local baseline
perf-target-compare target name:
    node scripts/perf/run.cjs --target {{target}} --compare-to {{name}}

# Start the counter-app example with the locally-built CLI
dev-example:
    cargo build -p thebe-cli
    cd examples/counter-app && ../../target/debug/thebe dev

dev-example-watch:
    cargo build -p thebe-cli
    cd examples/counter-app && ../../target/debug/thebe dev --watch

update-cli:
  cargo install --path crates/thebe-cli --force

# Install `thebe-lsp` to PATH (makes it available to editors via `thebe-lsp`)
update-lsp:
  cargo install --path crates/thebe-lsp --force

# Run the VS Code extension smoke tests
test-vscode:
    npm --prefix packages/thebe-vscode test

# Build `thebe-lsp` and copy it into the VS Code extension's bin/ directory
bundle-lsp:
  cargo build -p thebe-lsp --release
  cp target/release/thebe-lsp packages/thebe-vscode/bin/thebe-lsp

# Package the VS Code extension as a VSIX
package-vscode:
    npm --prefix packages/thebe-vscode run package:vsix

# Scaffold a new project (usage: just new my-app)
new name:
    cargo build -p thebe-cli
    target/debug/thebe new {{name}}
