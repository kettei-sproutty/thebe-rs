use clap::{Parser, Subcommand};

mod commands;
mod hotpatch;
mod tailwind;

#[derive(Parser)]
#[command(
  name = "thebe",
  about = "Thebe \u{2014} compiler-driven Rust web framework",
  version
)]
struct Cli {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  /// Start the development server (generate code and run `cargo run`).
  Dev {
    /// Watch `.trs` files and auto-restart the server on changes.
    #[arg(long, short, conflicts_with = "hotpatch")]
    watch: bool,
    /// Start a hotpatch-managed dev loop with in-place `.trs` updates and restart fallback.
    #[arg(long, conflicts_with = "watch")]
    hotpatch: bool,
  },
  /// Generate code and compile a release binary with `cargo build --release`.
  Build,
  /// Validate the project and emit `.thebe/diagnostics.json`.
  Check,
  /// Scaffold a new Thebe project.
  New {
    /// Name of the project directory to create.
    name: String,
  },
}

fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();
  match cli.command {
    Command::Dev { watch, hotpatch } => commands::dev::run(watch, hotpatch),
    Command::Build => commands::dev::build(),
    Command::Check => commands::dev::check(),
    Command::New { name } => commands::new::run(&name),
  }
}
