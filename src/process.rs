use std::process::Command;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitOutcome {
    pub code: i32,
}

pub trait CommandRunner {
    fn run(
        &self,
        program: &str,
        args: &[String],
        envs: &[(String, String)],
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
        envs: &[(String, String)],
    ) -> Result<ExitOutcome, ProcessError> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        for (name, value) in envs {
            cmd.env(name, value);
        }

        let status = cmd.status().map_err(ProcessError::Launch)?;
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
