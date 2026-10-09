// SPDX-License-Identifier: MPL-2.0

//! 入力の検証・確認・結果表示を担当する。Keychain の保存先選択と操作はストア層に委ねる。

use std::io::{self, BufRead, Write};

use thiserror::Error;

use crate::cli::{
    Cli, Command, DeleteArgs, HistoryArgs, HistoryCommand, RunArgs, SetArgs, StatusArgs,
};
use crate::history::{
    DisabledHistory, Entry, EntryKind, ErrorClass, Event, History, HistoryError, KeyName,
    MutationOperation, OperationId, Outcome, ProgramName, Query, Record, RunResult, Settings,
    SettingsUpdate, Termination,
};
use crate::process::{CommandRunner, ExitOutcome, ProcessError};
use crate::store::{CredentialStore, Secret, StoreError};

pub trait Prompter {
    fn prompt_secret(&mut self, prompt: &str) -> Result<Secret, AppError>;
    fn confirm(&mut self, prompt: &str) -> Result<bool, AppError>;
}

#[derive(Default)]
pub struct StdioPrompter;

impl Prompter for StdioPrompter {
    fn prompt_secret(&mut self, prompt: &str) -> Result<Secret, AppError> {
        // 値はエコーしない対話入力で受け取り、保持したバッファを解放時に消去する Secret に渡す。
        let value = rpassword::prompt_password(prompt).map_err(AppError::Io)?;
        Ok(Secret::new(value))
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool, AppError> {
        confirm_with_io(prompt, &mut io::stdin().lock(), &mut io::stdout().lock())
    }
}

fn confirm_with_io(
    prompt: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<bool, AppError> {
    output.write_all(prompt.as_bytes())?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub struct App {
    store: Box<dyn CredentialStore>,
    prompter: Box<dyn Prompter>,
    runner: Box<dyn CommandRunner>,
    history: Box<dyn History>,
}

impl App {
    pub fn new(
        store: Box<dyn CredentialStore>,
        prompter: Box<dyn Prompter>,
        runner: Box<dyn CommandRunner>,
    ) -> Self {
        Self {
            store,
            prompter,
            runner,
            history: Box::new(DisabledHistory),
        }
    }

    /// 履歴はストアと独立して注入し、status では一切アクセスしない。
    pub fn with_history(mut self, history: Box<dyn History>) -> Self {
        self.history = history;
        self
    }

    pub fn run(
        &mut self,
        cli: Cli,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> Result<i32, AppError> {
        match cli.command {
            Command::Set(args) => {
                self.handle_set(args, out, err)?;
                Ok(0)
            }
            Command::Status(args) => {
                self.handle_status(args, out)?;
                Ok(0)
            }
            Command::Run(args) => self.handle_run(args, err),
            Command::Delete(args) => {
                self.handle_delete(args, out, err)?;
                Ok(0)
            }
            Command::History(args) => {
                self.handle_history(args, out)?;
                Ok(0)
            }
        }
    }

    fn handle_set(
        &mut self,
        args: SetArgs,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> Result<(), AppError> {
        let id = history_id(err);
        let mut operation = MutationOperation::Set;
        let mut key = None;
        // この結果に表示 I/O を含めない。保存 API 成功後の表示失敗でも履歴は成功を保つ。
        let result = (|| -> Result<bool, AppError> {
            validate_env_name(&args.env_name)?;
            key = KeyName::new(&args.env_name);
            let exists = self.store.exists(&args.env_name)?;
            operation = if exists {
                MutationOperation::Replace
            } else {
                MutationOperation::Create
            };
            if exists && !self.confirm_replacement(&args.env_name)? {
                return Ok(false);
            }
            let secret = self
                .prompter
                .prompt_secret(&format!("Enter secret for {}: ", args.env_name))?;
            if exists {
                self.store.replace(&args.env_name, &secret)?;
            } else {
                match self.store.create(&args.env_name, &secret) {
                    Ok(()) => {}
                    Err(StoreError::AlreadyExists) => {
                        operation = MutationOperation::Replace;
                        if !self.confirm_replacement(&args.env_name)? {
                            return Ok(false);
                        }
                        self.store.replace(&args.env_name, &secret)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(true)
        })();
        record_history(
            self.history.as_ref(),
            id.as_ref(),
            Event::Mutation {
                operation,
                key,
                outcome: mutation_outcome(&result),
            },
            err,
        );
        if result? {
            // Stored は保存 API の成功を示す。API キー自体の有効性を保証するものではない。
            writeln!(out, "Stored '{}'.", args.env_name)?;
        } else {
            writeln!(out, "Aborted.")?;
        }
        Ok(())
    }

    fn confirm_replacement(&mut self, name: &str) -> Result<bool, AppError> {
        let question = format!("Entry '{name}' already exists. Replace it? [y/N]: ");
        self.prompter.confirm(&question)
    }

    fn handle_status(&self, args: StatusArgs, out: &mut dyn Write) -> Result<(), AppError> {
        if let Some(name) = args.env_name {
            validate_env_name(&name)?;
            if self.store.exists(&name)? {
                writeln!(out, "{name}: registered").map_err(AppError::Io)?;
            } else {
                writeln!(out, "{name}: missing").map_err(AppError::Io)?;
            }
            return Ok(());
        }

        let entries = self.store.list_names()?;
        if entries.is_empty() {
            writeln!(out, "No registered keys.").map_err(AppError::Io)?;
            return Ok(());
        }

        for name in entries {
            writeln!(out, "{name}").map_err(AppError::Io)?;
        }
        Ok(())
    }

    fn handle_run(&self, args: RunArgs, err: &mut dyn Write) -> Result<i32, AppError> {
        let id = history_id(err);
        let keys: Vec<KeyName> = args
            .keys
            .iter()
            .filter_map(|key| KeyName::new(key))
            .collect();
        let program_name = args
            .command
            .first()
            .and_then(|program| ProgramName::from_path(program));
        let prepare = (|| -> Result<_, AppError> {
            for key in &args.keys {
                validate_env_name(key)?;
            }
            let program = args
                .command
                .first()
                .ok_or_else(|| AppError::InvalidCommand("missing program".to_string()))?;
            let mut envs = Vec::with_capacity(args.keys.len());
            for key in &args.keys {
                envs.push((key.clone(), self.store.get(key)?));
            }
            Ok((program, envs))
        })();
        let (program, envs) = match prepare {
            Ok(prepared) => prepared,
            Err(error) => {
                record_history(
                    self.history.as_ref(),
                    id.as_ref(),
                    Event::RunFailure {
                        keys,
                        program: program_name,
                        error: error_class(&error),
                    },
                    err,
                );
                return Err(error);
            }
        };
        // callback は spawn 成功時だけ呼ばれる。秘密値、引数、環境変数は capture しない。
        let history = self.history.as_ref();
        let mut on_started = || {
            record_history(
                history,
                id.as_ref(),
                Event::RunStart {
                    keys: keys.clone(),
                    program: program_name.clone(),
                },
                err,
            )
        };
        let result = self
            .runner
            .run(program, &args.command[1..], &envs, &mut on_started);
        match result {
            Ok(outcome) => {
                let termination = match outcome {
                    ExitOutcome::Exited(code) => Termination::Exited(code),
                    ExitOutcome::Signaled(signal) => Termination::Signaled(signal),
                    ExitOutcome::Unknown => Termination::Unknown,
                };
                record_history(
                    self.history.as_ref(),
                    id.as_ref(),
                    Event::RunEnd {
                        keys,
                        program: program_name,
                        termination,
                    },
                    err,
                );
                Ok(outcome.shell_code())
            }
            Err(ProcessError::Launch(inner)) => {
                record_history(
                    self.history.as_ref(),
                    id.as_ref(),
                    Event::RunFailure {
                        keys,
                        program: program_name,
                        error: ErrorClass::Launch,
                    },
                    err,
                );
                let _ = writeln!(err, "failed to launch child process");
                Err(ProcessError::Launch(inner).into())
            }
            Err(ProcessError::Wait(inner)) => {
                record_history(
                    self.history.as_ref(),
                    id.as_ref(),
                    Event::RunEnd {
                        keys,
                        program: program_name,
                        termination: Termination::Unknown,
                    },
                    err,
                );
                Err(ProcessError::Wait(inner).into())
            }
        }
    }

    fn handle_delete(
        &mut self,
        args: DeleteArgs,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> Result<(), AppError> {
        let id = history_id(err);
        let mut key = None;
        let result = (|| -> Result<Option<bool>, AppError> {
            validate_env_name(&args.env_name)?;
            key = KeyName::new(&args.env_name);
            let question = format!(
                "Delete '{}' from kagitaba keychain entries? [y/N]: ",
                args.env_name
            );
            if !self.prompter.confirm(&question)? {
                return Ok(None);
            }
            Ok(Some(self.store.delete(&args.env_name)?))
        })();
        let outcome = match &result {
            Ok(Some(true)) => Outcome::Success,
            Ok(Some(false)) => Outcome::Failure(ErrorClass::NotFound),
            Ok(None) => Outcome::Aborted,
            Err(error) => error_outcome(error),
        };
        record_history(
            self.history.as_ref(),
            id.as_ref(),
            Event::Mutation {
                operation: MutationOperation::Delete,
                key,
                outcome,
            },
            err,
        );
        match result? {
            Some(true) => writeln!(out, "Deleted '{}'.", args.env_name)?,
            Some(false) => writeln!(out, "'{}' not found.", args.env_name)?,
            None => writeln!(out, "Aborted.")?,
        }
        Ok(())
    }

    fn handle_history(&mut self, args: HistoryArgs, out: &mut dyn Write) -> Result<(), AppError> {
        match args.command {
            Some(HistoryCommand::Enable) => print_settings(
                self.history.configure(SettingsUpdate {
                    enabled: Some(true),
                    ..SettingsUpdate::default()
                })?,
                out,
            )?,
            Some(HistoryCommand::Disable) => print_settings(
                self.history.configure(SettingsUpdate {
                    enabled: Some(false),
                    ..SettingsUpdate::default()
                })?,
                out,
            )?,
            Some(HistoryCommand::Config(config)) => {
                let settings = if config.retention_days.is_none() && config.max_events.is_none() {
                    self.history.settings()?
                } else {
                    self.history.configure(SettingsUpdate {
                        retention_days: config.retention_days,
                        max_events: config.max_events,
                        enabled: None,
                    })?
                };
                print_settings(settings, out)?;
            }
            Some(HistoryCommand::Clear) => {
                if !self
                    .prompter
                    .confirm("Clear all kagitaba operation history? [y/N]: ")?
                {
                    writeln!(out, "Aborted.")?;
                    return Ok(());
                }
                self.history.clear()?;
                writeln!(out, "History cleared.")?;
            }
            Some(HistoryCommand::Reclaim) => {
                self.history.reclaim()?;
                writeln!(out, "History storage reclaimed.")?;
            }
            None => {
                let key = args
                    .key
                    .as_deref()
                    .map(|name| {
                        validate_env_name(name)?;
                        KeyName::new(name).ok_or(AppError::InvalidEnvName)
                    })
                    .transpose()?;
                let entries = self.history.list(&Query {
                    key,
                    failed: args.failed,
                    limit: args.limit,
                })?;
                if entries.is_empty() {
                    writeln!(out, "No operation history.")?;
                }
                for entry in entries {
                    print_entry(&entry, out)?;
                }
            }
        }
        Ok(())
    }
}

const HISTORY_WARNING: &str = "warning: operation history could not be recorded.";

fn history_id(err: &mut dyn Write) -> Option<OperationId> {
    match OperationId::new() {
        Ok(id) => Some(id),
        Err(_) => {
            let _ = writeln!(err, "{HISTORY_WARNING}");
            None
        }
    }
}

fn record_history(
    history: &dyn History,
    id: Option<&OperationId>,
    event: Event,
    err: &mut dyn Write,
) {
    if let Some(id) = id
        && history
            .record(&Record {
                operation_id: id.clone(),
                event,
            })
            .is_err()
    {
        let _ = writeln!(err, "{HISTORY_WARNING}");
    }
}

fn error_class(error: &AppError) -> ErrorClass {
    match error {
        AppError::InvalidEnvName | AppError::InvalidCommand(_) => ErrorClass::InvalidInput,
        AppError::Store(error) => match error {
            StoreError::NotFound => ErrorClass::NotFound,
            StoreError::AccessDenied => ErrorClass::AccessDenied,
            StoreError::OperationCanceled => ErrorClass::Canceled,
            StoreError::InteractionUnavailable => ErrorClass::InteractionUnavailable,
            StoreError::UnsupportedPlatform => ErrorClass::Unsupported,
            StoreError::AlreadyExists | StoreError::Backend => ErrorClass::Backend,
        },
        AppError::Io(_) => ErrorClass::PromptIo,
        AppError::Process(ProcessError::Launch(_)) => ErrorClass::Launch,
        AppError::Process(ProcessError::Wait(_)) => ErrorClass::Wait,
        AppError::History(_) => ErrorClass::Backend,
    }
}

fn error_outcome(error: &AppError) -> Outcome {
    let class = error_class(error);
    if class == ErrorClass::Canceled {
        Outcome::Aborted
    } else {
        Outcome::Failure(class)
    }
}

fn mutation_outcome(result: &Result<bool, AppError>) -> Outcome {
    match result {
        Ok(true) => Outcome::Success,
        Ok(false) => Outcome::Aborted,
        Err(error) => error_outcome(error),
    }
}

fn print_settings(settings: Settings, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "enabled: {}", settings.enabled)?;
    writeln!(out, "retention_days: {}", settings.retention_days)?;
    writeln!(out, "max_events: {}", settings.max_events)
}

fn print_entry(entry: &Entry, out: &mut dyn Write) -> io::Result<()> {
    let timestamp =
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(entry.utc_ms) * 1_000_000)
            .ok()
            .and_then(|date| {
                date.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_else(|| "unknown UTC".into());
    write!(out, "{} {} ", timestamp, entry.operation_id.as_str())?;
    match &entry.kind {
        EntryKind::Mutation {
            operation,
            key,
            outcome,
        } => {
            write!(
                out,
                "{operation:?} key={} ",
                key.as_ref().map_or("-", KeyName::as_str)
            )?;
            match outcome {
                Outcome::Success => writeln!(out, "success"),
                Outcome::Aborted => writeln!(out, "aborted"),
                Outcome::Failure(error) => writeln!(out, "failure={}", error.as_str()),
            }
        }
        EntryKind::Run {
            keys,
            program,
            result,
        } => {
            let names = keys
                .iter()
                .map(KeyName::as_str)
                .collect::<Vec<_>>()
                .join(",");
            write!(
                out,
                "Run program={} keys={} ",
                program.as_ref().map_or("-", ProgramName::as_str),
                names
            )?;
            match result {
                RunResult::Failed(error) => writeln!(out, "failure={}", error.as_str()),
                RunResult::Finished(termination) => {
                    writeln!(out, "{}", termination_text(*termination))
                }
                RunResult::Unknown => writeln!(out, "unknown (no end recorded)"),
                RunResult::EndOnly(termination) => {
                    writeln!(out, "end-only {}", termination_text(*termination))
                }
            }
        }
    }
}

fn termination_text(termination: Termination) -> String {
    match termination {
        Termination::Exited(code) => format!("exit={code}"),
        Termination::Signaled(signal) => format!("signal={signal}"),
        Termination::Unknown => "unknown termination".into(),
    }
}

fn validate_env_name(input: &str) -> Result<(), AppError> {
    // 環境変数としての構文を制限し、拒否した入力自体はエラーへ含めない。
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return Err(AppError::InvalidEnvName);
    };
    if !(first == '_' || first.is_ascii_uppercase()) {
        return Err(AppError::InvalidEnvName);
    }
    if chars.any(|c| !(c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit())) {
        return Err(AppError::InvalidEnvName);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(
        "invalid environment variable name; use uppercase letters, digits and underscores, starting with a letter or underscore"
    )]
    InvalidEnvName,
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    History(#[from] HistoryError),
}

#[cfg(test)]
mod tests;
