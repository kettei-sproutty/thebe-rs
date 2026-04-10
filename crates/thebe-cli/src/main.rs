use clap::{Parser, Subcommand};

mod commands;

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
    Dev,
    /// Scaffold a new Thebe project.
    New {
        /// Name of the project directory to create.
        name: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Dev => commands::dev::run(),
        Command::New { name } => commands::new::run(&name),
    }
}
