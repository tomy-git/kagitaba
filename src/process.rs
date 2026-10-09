// SPDX-License-Identifier: MPL-2.0

use std::process::Command;

use thiserror::Error;

use crate::store::Secret;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitOutcome {
    Exited(i32),
    Signaled(i32),
    Unknown,
}

impl ExitOutcome {
    pub fn shell_code(self) -> i32 {
        match self {
            Self::Exited(code) => code,
            Self::Signaled(signal) => 128 + signal,
            Self::Unknown => 1,
        }
    }
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
        let mut started = 0;
        let outcome = SystemCommandRunner
            .run("/bin/sh", &args, &envs, &mut || started += 1)
            .unwrap();
        assert_eq!(outcome, ExitOutcome::Exited(42));
        assert_eq!(started, 1);
        assert_eq!(std::env::var_os(name), original);
    }

    #[test]
    fn propagates_signal_termination() {
        let outcome = SystemCommandRunner
            .run(
                "/bin/sh",
                &["-c".into(), "kill -TERM $$".into()],
                &[],
                &mut || {},
            )
            .unwrap();
        assert_eq!(outcome, ExitOutcome::Signaled(15));
        assert_eq!(outcome.shell_code(), 143);
    }

    #[test]
    fn failed_launch_does_not_include_secret() {
        let envs = vec![("TEST_KEY".into(), Secret::new("synthetic-value".into()))];
        let mut started = false;
        let error = SystemCommandRunner
            .run(
                "/nonexistent/kagitaba-test-program",
                &[],
                &envs,
                &mut || started = true,
            )
            .unwrap_err();
        assert!(!error.to_string().contains("synthetic-value"));
        assert!(!started);
    }

    #[test]
    fn explicit_exit_143_is_distinct_from_a_signal() {
        let outcome = SystemCommandRunner
            .run(
                "/bin/sh",
                &["-c".into(), "exit 143".into()],
                &[],
                &mut || {},
            )
            .unwrap();
        assert_eq!(outcome, ExitOutcome::Exited(143));
        assert_eq!(outcome.shell_code(), 143);
    }
}

pub trait CommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        envs: &[(String, Secret)],
        on_started: &mut dyn FnMut(),
    ) -> Result<ExitOutcome, ProcessError>;
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to launch child process")]
    Launch(#[source] std::io::Error),
    #[error("failed to wait for child process")]
    Wait(#[source] std::io::Error),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        envs: &[(String, Secret)],
        on_started: &mut dyn FnMut(),
    ) -> Result<ExitOutcome, ProcessError> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        for (name, value) in envs {
            cmd.env(name, value.expose());
        }

        // Drop Command's environment copies as soon as the child has been spawned.
        let mut child = cmd.spawn().map_err(ProcessError::Launch)?;
        drop(cmd);
        // 起動成功の事実だけを通知する。callback は秘密値や引数を受け取らない。
        on_started();
        let status = child.wait().map_err(ProcessError::Wait)?;
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(code) = status.code() {
                return Ok(ExitOutcome::Exited(code));
            }
            if let Some(signal) = status.signal() {
                return Ok(ExitOutcome::Signaled(signal));
            }
        }

        Ok(status
            .code()
            .map(ExitOutcome::Exited)
            .unwrap_or(ExitOutcome::Unknown))
    }
}
