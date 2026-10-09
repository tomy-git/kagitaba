// SPDX-License-Identifier: MPL-2.0

use super::{paths::PrivateDirectory, *};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};
use std::os::fd::OwnedFd;

pub(super) const FILE: &str = "history.sqlite3";
const EVENTS: &str = "CREATE TABLE events (id INTEGER PRIMARY KEY, operation_id TEXT NOT NULL, utc_ms INTEGER NOT NULL, kind TEXT NOT NULL, mutation TEXT, outcome TEXT, error_class TEXT, program TEXT, exit_code INTEGER, signal INTEGER, UNIQUE(operation_id, kind))";
const KEYS: &str = "CREATE TABLE event_keys (event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE, key_name TEXT NOT NULL, PRIMARY KEY(event_id, key_name))";
const INDEX: &str = "CREATE INDEX events_time ON events(utc_ms, id)";

pub(super) fn error(error: rusqlite::Error) -> HistoryError {
    match error {
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            HistoryError::Busy
        }
        _ => HistoryError::Database,
    }
}

pub(super) struct Database<'a> {
    pub connection: Connection,
    directory: &'a PrivateDirectory,
    original: OwnedFd,
}

impl<'a> Database<'a> {
    pub fn open(
        directory: &'a PrivateDirectory,
        create: bool,
    ) -> Result<Option<Self>, HistoryError> {
        let original = match directory.open_file(FILE, false, false)? {
            Some(fd) => fd,
            None if !create => return Ok(None),
            None => match directory.open_file(FILE, true, true) {
                Ok(Some(fd)) => fd,
                // Another cooperating process may have created it first.
                Err(HistoryError::Unavailable) => directory
                    .open_file(FILE, false, false)?
                    .ok_or(HistoryError::Unavailable)?,
                Ok(None) => return Err(HistoryError::Unavailable),
                Err(error) => return Err(error),
            },
        };
        directory.validate_children()?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut connection =
            Connection::open_with_flags(directory.path.join(FILE), flags).map_err(error)?;
        directory.identity(FILE, &original)?;
        connection
            .busy_timeout(std::time::Duration::from_millis(250))
            .map_err(error)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY; PRAGMA synchronous=FULL;").map_err(error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        schema(&transaction)?;
        transaction.commit().map_err(error)?;
        connection
            .execute_batch("PRAGMA journal_mode=DELETE")
            .map_err(error)?;
        let database = Self {
            connection,
            directory,
            original,
        };
        database.check()?;
        Ok(Some(database))
    }

    pub fn check(&self) -> Result<(), HistoryError> {
        self.directory.identity(FILE, &self.original)?;
        self.directory.validate_children()
    }

    pub fn reclaim(&self) -> Result<(), HistoryError> {
        self.connection.execute_batch("VACUUM").map_err(error)?;
        self.check()
    }
}

fn schema(transaction: &Transaction<'_>) -> Result<(), HistoryError> {
    let version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(error)?;
    let mut statement = transaction.prepare("SELECT type, name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name").map_err(error)?;
    let objects: Vec<(String, String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(error)?;
    drop(statement);
    if version == 0 && objects.is_empty() {
        transaction.execute_batch(EVENTS).map_err(error)?;
        transaction.execute_batch(KEYS).map_err(error)?;
        transaction.execute_batch(INDEX).map_err(error)?;
        transaction
            .pragma_update(None, "user_version", 1)
            .map_err(error)?;
    } else if version != 1
        || objects
            != [
                ("table".into(), "event_keys".into(), KEYS.into()),
                ("table".into(), "events".into(), EVENTS.into()),
                ("index".into(), "events_time".into(), INDEX.into()),
            ]
    {
        return Err(HistoryError::Database);
    }
    Ok(())
}

fn mutation_string(operation: MutationOperation) -> &'static str {
    match operation {
        MutationOperation::Set => "set",
        MutationOperation::Create => "create",
        MutationOperation::Replace => "replace",
        MutationOperation::Delete => "delete",
    }
}

fn error_class(value: &str) -> Result<ErrorClass, HistoryError> {
    [
        ErrorClass::InvalidInput,
        ErrorClass::NotFound,
        ErrorClass::AccessDenied,
        ErrorClass::Canceled,
        ErrorClass::InteractionUnavailable,
        ErrorClass::Unsupported,
        ErrorClass::Backend,
        ErrorClass::PromptIo,
        ErrorClass::Launch,
        ErrorClass::Wait,
    ]
    .into_iter()
    .find(|class| class.as_str() == value)
    .ok_or(HistoryError::Database)
}

pub(super) fn record(
    connection: &mut Connection,
    record: &Record,
    settings: Settings,
    now: i64,
) -> Result<(), HistoryError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    let (kind, mutation, outcome, error_value, program, exit, signal, keys) = match &record.event {
        Event::Mutation {
            operation,
            key,
            outcome,
        } => {
            let (outcome, error) = match outcome {
                Outcome::Success => ("success", None),
                Outcome::Aborted => ("aborted", None),
                Outcome::Failure(class) => ("failure", Some(class.as_str())),
            };
            (
                "mutation",
                Some(mutation_string(*operation)),
                Some(outcome),
                error,
                None,
                None,
                None,
                key.iter().collect::<Vec<_>>(),
            )
        }
        Event::RunFailure {
            keys,
            program,
            error,
        } => (
            "run_failure",
            None,
            None,
            Some(error.as_str()),
            program.as_ref().map(ProgramName::as_str),
            None,
            None,
            keys.iter().collect(),
        ),
        Event::RunStart { keys, program } => (
            "run_start",
            None,
            None,
            None,
            program.as_ref().map(ProgramName::as_str),
            None,
            None,
            keys.iter().collect(),
        ),
        Event::RunEnd {
            keys,
            program,
            termination,
        } => {
            let (exit, signal) = match termination {
                Termination::Exited(code) => (Some(*code), None),
                Termination::Signaled(signal) => (None, Some(*signal)),
                Termination::Unknown => (None, None),
            };
            (
                "run_end",
                None,
                None,
                None,
                program.as_ref().map(ProgramName::as_str),
                exit,
                signal,
                keys.iter().collect(),
            )
        }
    };
    transaction.execute("INSERT INTO events (operation_id, utc_ms, kind, mutation, outcome, error_class, program, exit_code, signal) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![record.operation_id.as_str(), now, kind, mutation, outcome, error_value, program, exit, signal]).map_err(error)?;
    let event_id = transaction.last_insert_rowid();
    for key in keys {
        transaction
            .execute(
                "INSERT OR IGNORE INTO event_keys (event_id, key_name) VALUES (?1,?2)",
                params![event_id, key.as_str()],
            )
            .map_err(error)?;
    }
    prune(&transaction, settings, now)?;
    transaction.commit().map_err(error)
}

fn prune(transaction: &Transaction<'_>, settings: Settings, now: i64) -> Result<(), HistoryError> {
    let cutoff = now.saturating_sub(i64::from(settings.retention_days) * 86_400_000);
    // A correlation group expires according to its newest event. Start/end pairs
    // are removed together; a late end after pruning remains an explicit EndOnly.
    transaction.execute("DELETE FROM events WHERE operation_id IN (SELECT operation_id FROM events GROUP BY operation_id HAVING MAX(utc_ms) < ?1)", [cutoff]).map_err(error)?;
    let mut statement = transaction.prepare("SELECT operation_id, COUNT(*) FROM events GROUP BY operation_id ORDER BY MAX(utc_ms) DESC, MAX(id) DESC").map_err(error)?;
    let groups: Vec<(String, u32)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(error)?
        .collect::<rusqlite::Result<_>>()
        .map_err(error)?;
    drop(statement);
    let mut count = 0u32;
    for (id, size) in groups {
        count = count.saturating_add(size);
        if count > settings.max_events {
            transaction
                .execute("DELETE FROM events WHERE operation_id = ?1", [id])
                .map_err(error)?;
        }
    }
    Ok(())
}

struct StoredEvent {
    id: i64,
    operation_id: OperationId,
    utc_ms: i64,
    kind: String,
    mutation: Option<String>,
    outcome: Option<String>,
    error: Option<String>,
    program: Option<ProgramName>,
    exit: Option<i32>,
    signal: Option<i32>,
    keys: Vec<KeyName>,
}

fn terminal(event: &StoredEvent) -> Result<Termination, HistoryError> {
    match (event.exit, event.signal) {
        (Some(code), None) => Ok(Termination::Exited(code)),
        (None, Some(signal)) => Ok(Termination::Signaled(signal)),
        (None, None) => Ok(Termination::Unknown),
        _ => Err(HistoryError::Database),
    }
}

pub(super) fn list(
    connection: &mut Connection,
    query: &Query,
    settings: Settings,
    now: i64,
) -> Result<Vec<Entry>, HistoryError> {
    if !(1..=1000).contains(&query.limit) {
        return Err(HistoryError::Configuration);
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    prune(&transaction, settings, now)?;
    // Parameters cover key filtering. Grouping is done before failed filtering
    // and the final limit, so start/end rows can never split due to the query.
    let mut statement = transaction.prepare("SELECT id, operation_id, utc_ms, kind, mutation, outcome, error_class, program, exit_code, signal FROM events WHERE operation_id IN (SELECT operation_id FROM events WHERE (?1 IS NULL OR operation_id IN (SELECT e.operation_id FROM events e JOIN event_keys k ON k.event_id=e.id WHERE k.key_name=?1)) GROUP BY operation_id HAVING (?2=0 OR MAX(CASE WHEN (kind='mutation' AND outcome='failure') OR kind='run_failure' OR (kind='run_end' AND (exit_code!=0 OR signal IS NOT NULL)) THEN 1 ELSE 0 END)=1) ORDER BY MAX(utc_ms) DESC, MAX(id) DESC LIMIT ?3) ORDER BY utc_ms, id").map_err(error)?;
    let raw = statement
        .query_map(
            params![
                query.key.as_ref().map(KeyName::as_str),
                query.failed,
                query.limit
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i32>>(8)?,
                    row.get::<_, Option<i32>>(9)?,
                ))
            },
        )
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    drop(statement);
    let mut groups = std::collections::BTreeMap::<String, Vec<StoredEvent>>::new();
    for (id, operation_id, utc_ms, kind, mutation, outcome, error_value, program, exit, signal) in
        raw
    {
        let mut keys = transaction
            .prepare("SELECT key_name FROM event_keys WHERE event_id=?1 ORDER BY key_name")
            .map_err(error)?;
        let keys: Vec<String> = keys
            .query_map([id], |row| row.get(0))
            .map_err(error)?
            .collect::<rusqlite::Result<_>>()
            .map_err(error)?;
        let keys = keys
            .into_iter()
            .map(|key| KeyName::new(&key).ok_or(HistoryError::Database))
            .collect::<Result<_, _>>()?;
        let parsed_id = OperationId::parse(&operation_id).ok_or(HistoryError::Database)?;
        let program = program
            .map(|name| {
                ProgramName::from_path(&name)
                    .filter(|parsed| parsed.as_str() == name)
                    .ok_or(HistoryError::Database)
            })
            .transpose()?;
        groups.entry(operation_id).or_default().push(StoredEvent {
            id,
            operation_id: parsed_id,
            utc_ms,
            kind,
            mutation,
            outcome,
            error: error_value,
            program,
            exit,
            signal,
            keys,
        });
    }
    let mut entries = Vec::new();
    for events in groups.into_values() {
        let newest = events
            .iter()
            .max_by_key(|event| (event.utc_ms, event.id))
            .ok_or(HistoryError::Database)?;
        let kind = if let Some(mutation) = events.iter().find(|event| event.kind == "mutation") {
            if events.len() != 1 || mutation.keys.len() > 1 {
                return Err(HistoryError::Database);
            }
            let operation = match mutation.mutation.as_deref() {
                Some("set") => MutationOperation::Set,
                Some("create") => MutationOperation::Create,
                Some("replace") => MutationOperation::Replace,
                Some("delete") => MutationOperation::Delete,
                _ => return Err(HistoryError::Database),
            };
            let outcome = match mutation.outcome.as_deref() {
                Some("success") => Outcome::Success,
                Some("aborted") => Outcome::Aborted,
                Some("failure") => Outcome::Failure(error_class(
                    mutation.error.as_deref().ok_or(HistoryError::Database)?,
                )?),
                _ => return Err(HistoryError::Database),
            };
            EntryKind::Mutation {
                operation,
                key: mutation.keys.first().cloned(),
                outcome,
            }
        } else {
            let start = events.iter().find(|event| event.kind == "run_start");
            let end = events.iter().find(|event| event.kind == "run_end");
            let failure = events.iter().find(|event| event.kind == "run_failure");
            if events.iter().any(|event| {
                !matches!(event.kind.as_str(), "run_start" | "run_end" | "run_failure")
            }) {
                return Err(HistoryError::Database);
            }
            let result = if let Some(failure) = failure {
                RunResult::Failed(error_class(
                    failure.error.as_deref().ok_or(HistoryError::Database)?,
                )?)
            } else if let Some(end) = end {
                if start.is_some() {
                    RunResult::Finished(terminal(end)?)
                } else {
                    RunResult::EndOnly(terminal(end)?)
                }
            } else {
                RunResult::Unknown
            };
            let details = end.or(failure).or(start).ok_or(HistoryError::Database)?;
            EntryKind::Run {
                keys: details.keys.clone(),
                program: details.program.clone(),
                result,
            }
        };
        let failed = match &kind {
            EntryKind::Mutation {
                outcome: Outcome::Failure(_),
                ..
            } => true,
            EntryKind::Run {
                result: RunResult::Failed(_),
                ..
            } => true,
            EntryKind::Run {
                result:
                    RunResult::Finished(Termination::Exited(code))
                    | RunResult::EndOnly(Termination::Exited(code)),
                ..
            } => *code != 0,
            EntryKind::Run {
                result:
                    RunResult::Finished(Termination::Signaled(_))
                    | RunResult::EndOnly(Termination::Signaled(_)),
                ..
            } => true,
            _ => false,
        };
        if !query.failed || failed {
            entries.push((
                newest.id,
                Entry {
                    utc_ms: newest.utc_ms,
                    operation_id: newest.operation_id.clone(),
                    kind,
                },
            ));
        }
    }
    entries.sort_by_key(|(id, entry)| std::cmp::Reverse((entry.utc_ms, *id)));
    transaction.commit().map_err(error)?;
    Ok(entries
        .into_iter()
        .take(query.limit as usize)
        .map(|(_, entry)| entry)
        .collect())
}
