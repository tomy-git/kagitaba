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
    /// Inspect and configure opt-in local operation history.
    History(HistoryArgs),
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

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub command: Option<HistoryCommand>,
    #[arg(long, value_name = "ENV_NAME")]
    pub key: Option<String>,
    #[arg(long)]
    pub failed: bool,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
}

#[derive(Debug, Subcommand)]
pub enum HistoryCommand {
    Enable,
    Disable,
    Config(HistoryConfigArgs),
    Clear,
    /// Reclaim free pages without deleting remaining history.
    Reclaim,
}

#[derive(Debug, Args)]
pub struct HistoryConfigArgs {
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=3650))]
    pub retention_days: Option<u32>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(2..=1_000_000))]
    pub max_events: Option<u32>,
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

    #[test]
    fn history_filters_and_management_are_separate_commands() {
        let cli = Cli::try_parse_from([
            "kagitaba", "history", "--key", "API_KEY", "--failed", "--limit", "20",
        ])
        .unwrap();
        let Command::History(args) = cli.command else {
            panic!("expected history")
        };
        assert_eq!(args.key.as_deref(), Some("API_KEY"));
        assert!(args.failed);
        assert_eq!(args.limit, 20);
        for args in [
            vec!["kagitaba", "history", "enable", "--key", "API_KEY"],
            vec!["kagitaba", "history", "--key", "API_KEY", "enable"],
            vec!["kagitaba", "history", "--limit", "0"],
            vec!["kagitaba", "history", "--limit", "1001"],
            vec!["kagitaba", "history", "config", "--retention-days", "0"],
            vec!["kagitaba", "history", "config", "--max-events", "1"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        for args in [
            vec!["kagitaba", "history", "enable"],
            vec!["kagitaba", "history", "disable"],
            vec!["kagitaba", "history", "clear"],
            vec!["kagitaba", "history", "reclaim"],
            vec!["kagitaba", "history", "config"],
            vec![
                "kagitaba",
                "history",
                "config",
                "--retention-days",
                "30",
                "--max-events",
                "200",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }
}
