// SPDX-License-Identifier: MPL-2.0

use std::process::Command;

use thiserror::Error;

use crate::store::Secret;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitOutcome {
    pub code: i32,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn injects_environment_and_preserves_arguments_and_exit_code() {
        let name = "KAGITABA_TEST_CHILD_SECRET";
        let original = std::env::var_os(name);
        let envs = vec![(name.to_string(), Secret::new("synthetic-value".into()))];
        let args = vec![
            "-c".into(),
            "test \"$KAGITABA_TEST_CHILD_SECRET\" = synthetic-value || exit 1; test \"$1\" = '$(exit 99); literal' || exit 2; exit 42".into(),
            "fixture".into(),
            "$(exit 99); literal".into(),
        ];
        let outcome = SystemCommandRunner.run("/bin/sh", &args, &envs).unwrap();
        assert_eq!(outcome.code, 42);
        assert_eq!(std::env::var_os(name), original);
    }

    #[test]
    fn propagates_signal_termination() {
        let outcome = SystemCommandRunner
            .run("/bin/sh", &["-c".into(), "kill -TERM $$".into()], &[])
            .unwrap();
        assert_eq!(outcome.code, 143);
    }

    #[test]
    fn failed_launch_does_not_include_secret() {
        let envs = vec![("TEST_KEY".into(), Secret::new("synthetic-value".into()))];
        let error = SystemCommandRunner
            .run("/nonexistent/kagitaba-test-program", &[], &envs)
            .unwrap_err();
        assert!(!error.to_string().contains("synthetic-value"));
    }
}

pub trait CommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        envs: &[(String, Secret)],
    ) -> Result<ExitOutcome, ProcessError>;
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to launch child process")]
    Launch(#[source] std::io::Error),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        envs: &[(String, Secret)],
    ) -> Result<ExitOutcome, ProcessError> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        for (name, value) in envs {
            cmd.env(name, value.expose());
        }

        // Drop Command's environment copies as soon as the child has been spawned.
        let mut child = cmd.spawn().map_err(ProcessError::Launch)?;
        drop(cmd);
        let status = child.wait().map_err(ProcessError::Launch)?;
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(code) = status.code() {
                return Ok(ExitOutcome { code });
            }
            if let Some(signal) = status.signal() {
                return Ok(ExitOutcome { code: 128 + signal });
            }
        }

        Ok(ExitOutcome {
            code: status.code().unwrap_or(1),
        })
    }
}
