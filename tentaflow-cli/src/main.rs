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
    /// Operacje na kamerach (rotate-key)
    Camera {
        #[command(subcommand)]
        sub: commands::camera::CameraCommand,
    },
    /// Rotate persistent HMAC keys (pickup_token, frame_url, recording_url)
    Keys {
        #[command(subcommand)]
        sub: commands::keys::KeysCommand,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        TopCommand::Addon { sub } => commands::addon::run(sub),
        TopCommand::Camera { sub } => commands::camera::run(sub),
        TopCommand::Keys { sub } => commands::keys::run(sub),
    }
}
