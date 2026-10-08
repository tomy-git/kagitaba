use std::io::{self, Write};

use thiserror::Error;

use crate::cli::{Cli, Command, DeleteArgs, RunArgs, SetArgs, StatusArgs};
use crate::process::{CommandRunner, ProcessError};
use crate::store::{CredentialStore, Secret, StoreError};

pub trait Prompter {
    fn prompt_secret(&mut self, prompt: &str) -> Result<Secret, AppError>;
    fn confirm(&mut self, prompt: &str) -> Result<bool, AppError>;
}

#[derive(Default)]
pub struct StdioPrompter;

impl Prompter for StdioPrompter {
    fn prompt_secret(&mut self, prompt: &str) -> Result<Secret, AppError> {
        let value = rpassword::prompt_password(prompt).map_err(AppError::Io)?;
        Ok(Secret::new(value))
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool, AppError> {
        print!("{prompt}");
        io::stdout().flush().map_err(AppError::Io)?;

        let mut input = String::new();
        io::stdin().read_line(&mut input).map_err(AppError::Io)?;
        Ok(matches!(
            input.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

pub struct App {
    store: Box<dyn CredentialStore>,
    prompter: Box<dyn Prompter>,
    runner: Box<dyn CommandRunner>,
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
        }
    }

    pub fn run(
        &mut self,
        cli: Cli,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> Result<i32, AppError> {
        match cli.command {
            Command::Set(args) => {
                self.handle_set(args, out)?;
                Ok(0)
            }
            Command::Status(args) => {
                self.handle_status(args, out)?;
                Ok(0)
            }
            Command::Run(args) => self.handle_run(args, err),
            Command::Delete(args) => {
                self.handle_delete(args, out)?;
                Ok(0)
            }
        }
    }

    fn handle_set(&mut self, args: SetArgs, out: &mut dyn Write) -> Result<(), AppError> {
        validate_env_name(&args.env_name)?;

        let replace_existing = self.store.exists(&args.env_name)?;
        if replace_existing && !self.confirm_replacement(&args.env_name, out)? {
            return Ok(());
        }

        let secret = self
            .prompter
            .prompt_secret(&format!("Enter secret for {}: ", args.env_name))?;
        if replace_existing {
            self.store.replace(&args.env_name, &secret)?;
        } else {
            match self.store.create(&args.env_name, &secret) {
                Ok(()) => {}
                Err(StoreError::AlreadyExists) => {
                    // Another invocation may have registered this name during input.
                    if !self.confirm_replacement(&args.env_name, out)? {
                        return Ok(());
                    }
                    self.store.replace(&args.env_name, &secret)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        writeln!(out, "Stored '{}'.", args.env_name).map_err(AppError::Io)?;
        Ok(())
    }

    fn confirm_replacement(&mut self, name: &str, out: &mut dyn Write) -> Result<bool, AppError> {
        let question = format!("Entry '{name}' already exists. Replace it? [y/N]: ");
        if self.prompter.confirm(&question)? {
            Ok(true)
        } else {
            writeln!(out, "Aborted.").map_err(AppError::Io)?;
            Ok(false)
        }
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
        for key in &args.keys {
            validate_env_name(key)?;
        }
        let program = args
            .command
            .first()
            .ok_or_else(|| AppError::InvalidCommand("missing program".to_string()))?;
        let mut envs = Vec::with_capacity(args.keys.len());
        for key in &args.keys {
            let secret = self.store.get(key)?;
            envs.push((key.clone(), secret));
        }

        let program_args = args.command[1..].to_vec();

        let outcome = self
            .runner
            .run(program, &program_args, &envs)
            .map_err(|e| match e {
                ProcessError::Launch(inner) => {
                    let _ = writeln!(err, "failed to launch '{program}': {inner}");
                    AppError::Process(ProcessError::Launch(inner))
                }
            })?;

        Ok(outcome.code)
    }

    fn handle_delete(&mut self, args: DeleteArgs, out: &mut dyn Write) -> Result<(), AppError> {
        validate_env_name(&args.env_name)?;
        let question = format!(
            "Delete '{}' from kagitaba keychain entries? [y/N]: ",
            args.env_name
        );
        if !self.prompter.confirm(&question)? {
            writeln!(out, "Aborted.").map_err(AppError::Io)?;
            return Ok(());
        }

        if self.store.delete(&args.env_name)? {
            writeln!(out, "Deleted '{}'.", args.env_name).map_err(AppError::Io)?;
        } else {
            writeln!(out, "'{}' not found.", args.env_name).map_err(AppError::Io)?;
        }
        Ok(())
    }
}

fn validate_env_name(input: &str) -> Result<(), AppError> {
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
}

#[cfg(test)]
mod tests;
