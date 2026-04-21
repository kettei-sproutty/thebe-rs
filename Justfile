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

# Build `thebe-lsp` and copy it into the VS Code extension's bin/ directory
bundle-lsp:
  cargo build -p thebe-lsp --release
  cp target/release/thebe-lsp packages/thebe-vscode/bin/thebe-lsp

# Scaffold a new project (usage: just new my-app)
new name:
    cargo build -p thebe-cli
    target/debug/thebe new {{name}}
