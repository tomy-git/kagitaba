// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::rc::Rc;

use super::{App, AppError, Prompter};
use crate::cli::{Cli, Command};
use crate::process::{CommandRunner, ExitOutcome, ProcessError};
use crate::store::{CredentialStore, Secret, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Operation {
    Exists,
    Get,
    Create,
    Replace,
    Delete,
    List,
}

#[derive(Default)]
pub struct StoreState {
    pub values: BTreeMap<String, String>,
    pub calls: Vec<(Operation, String)>,
    pub failures: BTreeMap<Operation, StoreError>,
    pub concurrent_value: Option<String>,
    pub disappear_on_replace: bool,
}

#[derive(Clone, Default)]
pub struct FakeStore(pub Rc<RefCell<StoreState>>);

impl FakeStore {
    pub fn seed(&self, name: &str, value: &str) {
        self.0.borrow_mut().values.insert(name.into(), value.into());
    }

    fn record(&self, operation: Operation, name: &str) -> Result<(), StoreError> {
        let mut state = self.0.borrow_mut();
        state.calls.push((operation, name.into()));
        match state.failures.get(&operation).copied() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl CredentialStore for FakeStore {
    fn exists(&self, name: &str) -> Result<bool, StoreError> {
        self.record(Operation::Exists, name)?;
        Ok(self.0.borrow().values.contains_key(name))
    }

    fn get(&self, name: &str) -> Result<Secret, StoreError> {
        self.record(Operation::Get, name)?;
        self.0
            .borrow()
            .values
            .get(name)
            .cloned()
            .map(Secret::new)
            .ok_or(StoreError::NotFound)
    }

    fn create(&self, name: &str, secret: &Secret) -> Result<(), StoreError> {
        self.record(Operation::Create, name)?;
        let mut state = self.0.borrow_mut();
        if let Some(value) = state.concurrent_value.take() {
            state.values.insert(name.into(), value);
        }
        if state.values.contains_key(name) {
            return Err(StoreError::AlreadyExists);
        }
        state.values.insert(name.into(), secret.expose().into());
        Ok(())
    }

    fn replace(&self, name: &str, secret: &Secret) -> Result<(), StoreError> {
        self.record(Operation::Replace, name)?;
        let mut state = self.0.borrow_mut();
        if state.disappear_on_replace {
            state.values.remove(name);
        }
        let value = state.values.get_mut(name).ok_or(StoreError::NotFound)?;
        *value = secret.expose().into();
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<bool, StoreError> {
        self.record(Operation::Delete, name)?;
        Ok(self.0.borrow_mut().values.remove(name).is_some())
    }

    fn list_names(&self) -> Result<Vec<String>, StoreError> {
        self.record(Operation::List, "")?;
        Ok(self.0.borrow().values.keys().cloned().collect())
    }
}

#[derive(Default)]
pub struct PromptState {
    pub answers: VecDeque<bool>,
    pub confirmations: usize,
    pub secret_inputs: usize,
    pub secret_error: Option<io::ErrorKind>,
    pub confirm_error: Option<io::ErrorKind>,
}

#[derive(Clone, Default)]
pub struct FakePrompter(pub Rc<RefCell<PromptState>>);

impl Prompter for FakePrompter {
    fn prompt_secret(&mut self, _: &str) -> Result<Secret, AppError> {
        let mut state = self.0.borrow_mut();
        state.secret_inputs += 1;
        if let Some(kind) = state.secret_error {
            return Err(io::Error::from(kind).into());
        }
        Ok(Secret::new("new-secret".into()))
    }

    fn confirm(&mut self, _: &str) -> Result<bool, AppError> {
        let mut state = self.0.borrow_mut();
        state.confirmations += 1;
        if let Some(kind) = state.confirm_error {
            return Err(io::Error::from(kind).into());
        }
        Ok(state.answers.pop_front().expect("unexpected confirmation"))
    }
}

pub type RunCapture = (String, Vec<String>, Vec<(String, String)>);

#[derive(Default)]
pub struct RunnerState {
    pub calls: Vec<RunCapture>,
    pub exit_code: i32,
    pub failure: Option<io::ErrorKind>,
}

#[derive(Clone, Default)]
pub struct FakeRunner(pub Rc<RefCell<RunnerState>>);

impl CommandRunner for FakeRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        envs: &[(String, Secret)],
    ) -> Result<ExitOutcome, ProcessError> {
        let mut state = self.0.borrow_mut();
        state.calls.push((
            program.into(),
            args.to_vec(),
            envs.iter()
                .map(|(name, secret)| (name.clone(), secret.expose().into()))
                .collect(),
        ));
        match state.failure {
            Some(kind) => Err(ProcessError::Launch(io::Error::from(kind))),
            None => Ok(ExitOutcome {
                code: state.exit_code,
            }),
        }
    }
}

pub struct Fixture {
    pub store: FakeStore,
    pub prompt: FakePrompter,
    pub runner: FakeRunner,
    pub out: Vec<u8>,
    pub err: Vec<u8>,
    app: App,
}

impl Default for Fixture {
    fn default() -> Self {
        let store = FakeStore::default();
        let prompt = FakePrompter::default();
        let runner = FakeRunner::default();
        let app = App::new(
            Box::new(store.clone()),
            Box::new(prompt.clone()),
            Box::new(runner.clone()),
        );
        Self {
            store,
            prompt,
            runner,
            out: Vec::new(),
            err: Vec::new(),
            app,
        }
    }
}

impl Fixture {
    pub fn run(&mut self, command: Command) -> Result<i32, AppError> {
        self.app.run(Cli { command }, &mut self.out, &mut self.err)
    }

    pub fn answer(&self, answer: bool) {
        self.prompt.0.borrow_mut().answers.push_back(answer);
    }

    pub fn calls(&self) -> Vec<Operation> {
        self.store
            .0
            .borrow()
            .calls
            .iter()
            .map(|(operation, _)| *operation)
            .collect()
    }

    pub fn assert_no_secret_output(&self) {
        for output in [&self.out, &self.err] {
            let text = String::from_utf8_lossy(output);
            assert!(!text.contains("new-secret"));
            assert!(!text.contains("old-secret"));
            assert!(!text.contains("concurrent-secret"));
        }
    }
}
