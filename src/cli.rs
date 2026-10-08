// SPDX-License-Identifier: MPL-2.0

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "kagitaba",
    about = "Local-first API key manager for macOS",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Set(SetArgs),
    Status(StatusArgs),
    Run(RunArgs),
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    pub env_name: String,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    pub env_name: Option<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long = "key", required = true, value_name = "ENV_NAME")]
    pub keys: Vec<String>,
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, value_name = "PROGRAM [ARGS...]")]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    pub env_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_rejects_secret_argument() {
        assert!(Cli::try_parse_from(["kagitaba", "set", "TEST_KEY", "synthetic-value"]).is_err());
    }

    #[test]
    fn run_parses_selected_keys_and_child_arguments() {
        let cli = Cli::try_parse_from([
            "kagitaba",
            "run",
            "--key",
            "KEY_A",
            "--key",
            "KEY_B",
            "--",
            "program",
            "--key",
            "literal argument",
        ])
        .unwrap();
        let Command::Run(args) = cli.command else {
            panic!("expected run")
        };
        assert_eq!(args.keys, ["KEY_A", "KEY_B"]);
        assert_eq!(args.command, ["program", "--key", "literal argument"]);
    }
}
