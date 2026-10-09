// SPDX-License-Identifier: MPL-2.0

use std::process;

use clap::Parser;
use kagitaba::app::{App, AppError, StdioPrompter};
use kagitaba::cli::Cli;
use kagitaba::history::HistoryStore;
use kagitaba::process::SystemCommandRunner;
use kagitaba::store::default_store;

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                error.exit();
            }
            _ => {
                // Clap's detailed errors may echo a mistakenly supplied secret argument.
                eprintln!(
                    "error: invalid command-line arguments. Use 'kagitaba --help' for usage."
                );
                eprintln!("Enter secret values at the interactive prompt, never as arguments.");
                process::exit(2);
            }
        },
    };
    let store = default_store();
    let mut app = App::new(
        store,
        Box::new(StdioPrompter),
        Box::new(SystemCommandRunner),
    )
    .with_history(Box::new(HistoryStore::from_home()));

    match app.run(cli, &mut std::io::stdout(), &mut std::io::stderr()) {
        Ok(code) => process::exit(code),
        Err(err) => {
            let _ = writeln_no_fail(&mut std::io::stderr(), &format_error(&err));
            process::exit(1);
        }
    }
}

fn format_error(err: &AppError) -> String {
    format!("error: {err}")
}

fn writeln_no_fail(stderr: &mut std::io::Stderr, message: &str) -> std::io::Result<()> {
    use std::io::Write;
    writeln!(stderr, "{message}")
}
