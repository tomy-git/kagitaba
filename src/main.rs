use std::process;

use clap::Parser;
use kagitaba::app::{App, AppError, StdioPrompter};
use kagitaba::cli::Cli;
use kagitaba::process::SystemCommandRunner;
use kagitaba::store::default_store;

fn main() {
    let cli = Cli::parse();
    let store = default_store();
    let mut app = App::new(
        store,
        Box::new(StdioPrompter),
        Box::new(SystemCommandRunner),
    );

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
