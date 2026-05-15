// =============================================================================
// Plik: tentaflow-cli/src/main.rs
// Opis: Punkt wejscia narzedzia CLI TentaFlow. Definiuje podkomendy clap
//       i deleguje do moduliow w `commands/`.
// =============================================================================

use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser, Debug)]
#[command(
    name = "tentaflow-cli",
    version,
    about = "Narzedzia developerskie TentaFlow (validate, package, ...)"
)]
struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Subcommand, Debug)]
enum TopCommand {
    /// Operacje na addonach (validate, w F1c: package, sign)
    Addon {
        #[command(subcommand)]
        sub: commands::addon::AddonCommand,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        TopCommand::Addon { sub } => commands::addon::run(sub),
    }
}
