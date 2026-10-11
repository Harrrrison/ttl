use std::io::Write;
use std::process::ExitCode;

use clap::Parser;
use ttm::cli::{Cli, Exit};

fn main() -> ExitCode {
    let cli = Cli::parse();

    // No subcommand means the user wants the interactive TUI; that takes over
    // the terminal, so it cannot share a return type with the CLI path.
    let Some(command) = cli.command else {
        let store = match cli.home {
            Some(dir) => ttm::store::Store::open(dir),
            None => ttm::store::Store::from_env(),
        };
        return match store {
            Ok(store) => match ttm::tui::run(store) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(format!("could not start the terminal UI: {e}")),
            },
            Err(e) => fail(e.to_string()),
        };
    };

    match ttm::cli::run(Cli {
        command: Some(command),
        ..cli
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let mut message = failure.message;
            if failure.code == Exit::NotFound {
                message.push_str("\nhint: `ttm list` shows what exists.");
            }
            eprintln!("ttm: {message}");
            ExitCode::from(failure.code as u8)
        }
    }
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    let mut err = std::io::stderr().lock();
    writeln!(err, "ttm: {message}").ok();
    ExitCode::FAILURE
}
