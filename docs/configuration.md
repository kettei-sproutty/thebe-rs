# Configuration (`thebe.toml`)

Thebe projects are configured using a `thebe.toml` file in the root of the workspace (next to your `Cargo.toml`). This file allows you to define build lifecycle hooks and integrated CSS tooling.

## The `[tailwind]` Block

Thebe has first-class, zero-dependency support for Tailwind CSS.

When you configure the `[tailwind]` block, `thebe dev` and `thebe build` will **automatically download the official Tailwind Standalone CLI binary** for your specific OS and architecture directly from GitHub, caching it globally.

**You do not need Node.js, `npm`, `npx`, or a `package.json` to use Tailwind in Thebe.**

```toml
[tailwind]
# The source CSS file containing @tailwind directives
input = "src/input.css"

# Where the compiled CSS should be written
# Make sure your app.html links to this file!
output = "public/global.css"
```

Because the Tailwind compiler runs as part of the Thebe build pipeline, your styles are instantly rebuilt whenever you change a `.trs` component during `thebe dev`. During `thebe build`, the standalone CLI is invoked with `--minify` so the emitted CSS is compressed for production.

## The `[hooks]` Block

You can execute arbitrary shell commands before the build starts or whenever files change across the development lifecycle.

```toml
[hooks]
# Runs once when `thebe dev` or `thebe build` starts, before codegen
pre_build = "echo 'Starting build...'"

# Runs every time the file watcher detects a change to a .trs file
on_change = "echo 'Generating new artifacts...'"
```

Hooks are executed using the system's default shell (`sh -c` on Unix, `cmd /C` on Windows). If a hook returns a non-zero exit code, the build process will abort or the dev server will print an error and wait for the next file change.
