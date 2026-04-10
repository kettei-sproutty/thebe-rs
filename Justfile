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

# Scaffold a new project (usage: just new my-app)
new name:
    cargo build -p thebe-cli
    target/debug/thebe new {{name}}
