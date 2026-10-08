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

        if self.store.exists(&args.env_name)? {
            let question = format!(
                "Entry '{}' already exists. Replace it? [y/N]: ",
                args.env_name
            );
            if !self.prompter.confirm(&question)? {
                writeln!(out, "Aborted.").map_err(AppError::Io)?;
                return Ok(());
            }
        }

        let secret = self
            .prompter
            .prompt_secret(&format!("Enter secret for {}: ", args.env_name))?;
        self.store.set(&args.env_name, &secret)?;
        writeln!(out, "Stored '{}'.", args.env_name).map_err(AppError::Io)?;
        Ok(())
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
        let mut envs = Vec::with_capacity(args.keys.len());
        for key in &args.keys {
            validate_env_name(key)?;
            let secret = self.store.get(key)?;
            envs.push((key.clone(), secret.expose().to_string()));
        }

        let program = args
            .command
            .first()
            .ok_or_else(|| AppError::InvalidCommand("missing program".to_string()))?;
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
        return Err(AppError::InvalidEnvName(input.to_string()));
    };
    if !(first == '_' || first.is_ascii_uppercase()) {
        return Err(AppError::InvalidEnvName(input.to_string()));
    }
    if chars.any(|c| !(c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit())) {
        return Err(AppError::InvalidEnvName(input.to_string()));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("invalid environment variable name '{0}'")]
    InvalidEnvName(String),
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
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use super::*;
    use crate::process::ExitOutcome;
    type RunCapture = (String, Vec<String>, Vec<(String, String)>);

    #[derive(Default, Clone)]
    struct MockStore {
        values: Rc<RefCell<BTreeMap<String, String>>>,
    }

    impl CredentialStore for MockStore {
        fn exists(&self, env_name: &str) -> Result<bool, StoreError> {
            Ok(self.values.borrow().contains_key(env_name))
        }

        fn get(&self, env_name: &str) -> Result<Secret, StoreError> {
            self.values
                .borrow()
                .get(env_name)
                .cloned()
                .map(Secret::new)
                .ok_or(StoreError::NotFound)
        }

        fn set(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError> {
            self.values
                .borrow_mut()
                .insert(env_name.to_string(), secret.expose().to_string());
            Ok(())
        }

        fn delete(&self, env_name: &str) -> Result<bool, StoreError> {
            Ok(self.values.borrow_mut().remove(env_name).is_some())
        }

        fn list_names(&self) -> Result<Vec<String>, StoreError> {
            Ok(self.values.borrow().keys().cloned().collect())
        }
    }

    #[derive(Default)]
    struct MockPrompter {
        confirms: Vec<bool>,
        secrets: Vec<String>,
    }

    impl Prompter for MockPrompter {
        fn prompt_secret(&mut self, _: &str) -> Result<Secret, AppError> {
            Ok(Secret::new(self.secrets.remove(0)))
        }

        fn confirm(&mut self, _: &str) -> Result<bool, AppError> {
            Ok(self.confirms.remove(0))
        }
    }

    #[derive(Default)]
    struct MockRunner {
        captured: Rc<RefCell<Vec<RunCapture>>>,
        exit_code: i32,
    }

    impl CommandRunner for MockRunner {
        fn run(
            &self,
            program: &str,
            args: &[String],
            envs: &[(String, String)],
        ) -> Result<ExitOutcome, ProcessError> {
            self.captured
                .borrow_mut()
                .push((program.to_string(), args.to_vec(), envs.to_vec()));
            Ok(ExitOutcome {
                code: self.exit_code,
            })
        }
    }

    fn build_app(store: MockStore, prompter: MockPrompter, runner: MockRunner) -> App {
        App::new(Box::new(store), Box::new(prompter), Box::new(runner))
    }

    #[test]
    fn run_injects_only_selected_keys_and_propagates_exit_code() {
        let store = MockStore::default();
        store
            .set("OPENAI_API_KEY", &Secret::new("secret-a".to_string()))
            .expect("set key");
        store
            .set("OTHER_KEY", &Secret::new("secret-b".to_string()))
            .expect("set key");

        let captured = Rc::new(RefCell::new(Vec::new()));
        let runner = MockRunner {
            captured: captured.clone(),
            exit_code: 42,
        };

        let mut app = build_app(store, MockPrompter::default(), runner);
        let cli = Cli {
            command: Command::Run(RunArgs {
                keys: vec!["OPENAI_API_KEY".to_string()],
                command: vec!["echo".to_string(), "hello".to_string()],
            }),
        };

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = app.run(cli, &mut out, &mut err).expect("run succeeds");

        assert_eq!(code, 42);
        let records = captured.borrow();
        let (program, args, envs) = &records[0];
        assert_eq!(program, "echo");
        assert_eq!(args, &vec!["hello".to_string()]);
        assert_eq!(
            envs,
            &vec![("OPENAI_API_KEY".to_string(), "secret-a".to_string())]
        );
    }

    #[test]
    fn set_and_delete_require_confirmation() {
        let store = MockStore::default();
        store
            .set("OPENAI_API_KEY", &Secret::new("old".to_string()))
            .expect("seed key");

        let mut app = build_app(
            store.clone(),
            MockPrompter {
                confirms: vec![false, true],
                secrets: vec!["new".to_string()],
            },
            MockRunner::default(),
        );

        let mut out = Vec::new();
        let mut err = Vec::new();

        app.run(
            Cli {
                command: Command::Set(SetArgs {
                    env_name: "OPENAI_API_KEY".to_string(),
                }),
            },
            &mut out,
            &mut err,
        )
        .expect("set should not fail");

        assert_eq!(
            store.get("OPENAI_API_KEY").expect("still present").expose(),
            "old"
        );

        app.run(
            Cli {
                command: Command::Delete(DeleteArgs {
                    env_name: "OPENAI_API_KEY".to_string(),
                }),
            },
            &mut out,
            &mut err,
        )
        .expect("delete should not fail");

        assert!(!store.exists("OPENAI_API_KEY").expect("lookup"));
    }

    #[test]
    fn invalid_env_name_is_rejected() {
        let mut app = build_app(
            MockStore::default(),
            MockPrompter::default(),
            MockRunner::default(),
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = app.run(
            Cli {
                command: Command::Set(SetArgs {
                    env_name: "bad-name".to_string(),
                }),
            },
            &mut out,
            &mut err,
        );

        assert!(matches!(result, Err(AppError::InvalidEnvName(_))));
    }

    #[test]
    fn status_does_not_print_secret_values() {
        let store = MockStore::default();
        store
            .set("OPENAI_API_KEY", &Secret::new("top-secret".to_string()))
            .expect("seed key");

        let mut app = build_app(store, MockPrompter::default(), MockRunner::default());
        let mut out = Vec::new();
        let mut err = Vec::new();

        app.run(
            Cli {
                command: Command::Status(StatusArgs { env_name: None }),
            },
            &mut out,
            &mut err,
        )
        .expect("status succeeds");

        let output = String::from_utf8(out).expect("utf8 output");
        assert!(output.contains("OPENAI_API_KEY"));
        assert!(!output.contains("top-secret"));
    }

    #[test]
    fn missing_key_returns_not_found_without_secret_leak() {
        let mut app = build_app(
            MockStore::default(),
            MockPrompter::default(),
            MockRunner::default(),
        );
        let mut out = Vec::new();
        let mut err = Vec::new();

        let result = app.run(
            Cli {
                command: Command::Run(RunArgs {
                    keys: vec!["MISSING_KEY".to_string()],
                    command: vec!["env".to_string()],
                }),
            },
            &mut out,
            &mut err,
        );

        assert!(matches!(result, Err(AppError::Store(StoreError::NotFound))));
        let err_output = String::from_utf8(err).expect("utf8");
        assert!(!err_output.contains("MISSING_KEY"));
    }
}
