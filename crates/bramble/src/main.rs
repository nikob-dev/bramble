mod cli;
mod config;
mod node;
mod tui;

use clap::{Parser, Subcommand};

use crate::config::Config;

#[derive(Parser)]
#[command(
    name = "bramble",
    version,
    about = "A pet-bank that teaches electronic payments"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the pet's bank node (HTTP + WebSocket API).
    Node,
    /// Open the terminal UI: your pet, the current lesson, and a lab shell.
    Tui,
    /// Inspect the pet-bank's ledger (read-only).
    Ledger {
        #[command(subcommand)]
        command: cli::LedgerCommand,
    },
    /// Continue past an explanation in the current lesson.
    Next,
    /// Ask your pet for a hint on the current task.
    Hint,
    /// Answer a quiz question, e.g. `bramble answer b`.
    Answer { choice: String },
    /// Print the pet's identity and version.
    Whoami,
}

fn main() -> anyhow::Result<()> {
    let config = Config::from_env();
    match Args::parse().command.unwrap_or(Command::Whoami) {
        Command::Node => node::run(config),
        Command::Tui => tui::run(config),
        Command::Ledger { command } => cli::ledger(&config, command),
        Command::Next => cli::acknowledge("Continuing…"),
        Command::Hint => cli::acknowledge("Asking your pet for a hint…"),
        Command::Answer { choice } => cli::acknowledge(&format!("You answered: {choice}")),
        Command::Whoami => {
            println!(
                "bramble {} (protocol v{}): {}, a pet-bank that teaches payments",
                env!("CARGO_PKG_VERSION"),
                bramble_core::PROTOCOL_VERSION,
                config.pet_name
            );
            Ok(())
        }
    }
}
