// SPDX-License-Identifier: MPL-2.0

//! 履歴に渡せる情報を限定する。秘密値、任意のエラー文、引数や環境変数は受け取らない。

use std::path::Path;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyName(String);

impl KeyName {
    pub fn new(input: &str) -> Option<Self> {
        let mut chars = input.chars();
        let first = chars.next()?;
        if input.len() > 256
            || !(first == '_' || first.is_ascii_uppercase())
            || chars.any(|c| !(c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit()))
        {
            return None;
        }
        Some(Self(input.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramName(String);

impl ProgramName {
    pub fn from_path(input: &str) -> Option<Self> {
        let basename = Path::new(input).file_name()?.to_str()?;
        if basename.len() > 256 {
            return None;
        }
        let safe: String = basename
            .chars()
            .flat_map(|c| {
                if c.is_control() || matches!(c, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect();
        (safe.len() <= 256).then_some(Self(safe))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationId(String);

impl OperationId {
    pub fn new() -> Result<Self, HistoryError> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| HistoryError::Unavailable)?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }
    pub fn parse(input: &str) -> Option<Self> {
        (input.len() == 32
            && input
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
        .then(|| Self(input.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationOperation {
    Set,
    Create,
    Replace,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    InvalidInput,
    NotFound,
    AccessDenied,
    Canceled,
    InteractionUnavailable,
    Unsupported,
    Backend,
    PromptIo,
    Launch,
    Wait,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::AccessDenied => "access_denied",
            Self::Canceled => "canceled",
            Self::InteractionUnavailable => "interaction_unavailable",
            Self::Unsupported => "unsupported",
            Self::Backend => "backend",
            Self::PromptIo => "prompt_io",
            Self::Launch => "launch",
            Self::Wait => "wait",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure(ErrorClass),
    Aborted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    Exited(i32),
    Signaled(i32),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Mutation {
        operation: MutationOperation,
        key: Option<KeyName>,
        outcome: Outcome,
    },
    RunFailure {
        keys: Vec<KeyName>,
        program: Option<ProgramName>,
        error: ErrorClass,
    },
    RunStart {
        keys: Vec<KeyName>,
        program: Option<ProgramName>,
    },
    RunEnd {
        keys: Vec<KeyName>,
        program: Option<ProgramName>,
        termination: Termination,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub operation_id: OperationId,
    pub event: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub retention_days: u32,
    pub max_events: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: 90,
            max_events: 10_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SettingsUpdate {
    pub enabled: Option<bool>,
    pub retention_days: Option<u32>,
    pub max_events: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Query {
    pub key: Option<KeyName>,
    pub failed: bool,
    pub limit: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunResult {
    Failed(ErrorClass),
    Finished(Termination),
    Unknown,
    EndOnly(Termination),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Mutation {
        operation: MutationOperation,
        key: Option<KeyName>,
        outcome: Outcome,
    },
    Run {
        keys: Vec<KeyName>,
        program: Option<ProgramName>,
        result: RunResult,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub utc_ms: i64,
    pub operation_id: OperationId,
    pub kind: EntryKind,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HistoryError {
    #[error("history storage is unavailable")]
    Unavailable,
    #[error("history files have unsafe ownership, permissions or links")]
    UnsafePath,
    #[error("history storage is busy")]
    Busy,
    #[error("history configuration is invalid")]
    Configuration,
    #[error("history database is corrupt or has an unsupported schema")]
    Database,
    #[error("history was cleared, but storage reclamation failed")]
    Reclaim,
}

pub trait History {
    fn record(&self, record: &Record) -> Result<(), HistoryError>;
    fn settings(&self) -> Result<Settings, HistoryError> {
        Ok(Settings::default())
    }
    fn configure(&self, _: SettingsUpdate) -> Result<Settings, HistoryError> {
        Err(HistoryError::Unavailable)
    }
    fn list(&self, _: &Query) -> Result<Vec<Entry>, HistoryError> {
        Ok(Vec::new())
    }
    fn clear(&self) -> Result<(), HistoryError> {
        Err(HistoryError::Unavailable)
    }
    fn reclaim(&self) -> Result<(), HistoryError> {
        Err(HistoryError::Unavailable)
    }
}

pub struct DisabledHistory;
impl History for DisabledHistory {
    fn record(&self, _: &Record) -> Result<(), HistoryError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_omits_unvalidated_input_and_full_program_paths() {
        for input in ["", "bad=value", "KEY\n", "lowercase", "9KEY"] {
            assert!(KeyName::new(input).is_none());
        }
        assert_eq!(KeyName::new("_API_KEY_2").unwrap().as_str(), "_API_KEY_2");
        assert!(KeyName::new(&"A".repeat(257)).is_none());
        assert_eq!(
            ProgramName::from_path("/synthetic-private-path/program")
                .unwrap()
                .as_str(),
            "program"
        );
        assert!(ProgramName::from_path("/").is_none());
    }

    #[test]
    fn program_control_characters_cannot_control_terminal_output() {
        let name = ProgramName::from_path("/tmp/program\n\u{1b}[2J\u{202e}").unwrap();
        assert!(!name.as_str().chars().any(char::is_control));
        assert!(!name.as_str().contains('\u{202e}'));
        assert_eq!(ProgramName::from_path(name.as_str()).unwrap(), name);
        assert!(ProgramName::from_path(&"\u{1b}".repeat(100)).is_none());
    }

    #[test]
    fn operation_ids_have_a_validated_storage_representation() {
        let id = OperationId::new().unwrap();
        assert_eq!(OperationId::parse(id.as_str()), Some(id));
        for invalid in ["", "\n", "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF", "../../secret"] {
            assert!(OperationId::parse(invalid).is_none());
        }
    }
}
