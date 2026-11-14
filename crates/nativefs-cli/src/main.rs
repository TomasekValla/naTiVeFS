//! naTiVeFS-cli: terminal client for TVFS. No arguments = full-screen interface; `--direct-no-cli-ui` = AI agent mode.

mod agent;
mod args;
mod common;
mod confirm;
mod expiry;
mod links;
mod settings;
mod tui;

use args::Command;

fn main() {
    let code = match args::parse(std::env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{}", args::HELP);
            0
        }
        Ok(Command::Version) => {
            println!("naTiVeFS-cli {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Ok(Command::AiHelp) => {
            print!("{}", tvfs_core::ai_help::AI_AGENT_HELP);
            0
        }
        Ok(Command::Agent(a)) => agent::run(a),
        Ok(Command::Tui { theme, paths }) => tui::run(theme, paths),
        Err(e) => {
            eprintln!("naTiVeFS-cli: {e}");
            agent::EXIT_USAGE
        }
    };
    std::process::exit(code);
}
