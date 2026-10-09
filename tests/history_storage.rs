// SPDX-License-Identifier: MPL-2.0

#![cfg(unix)]

use kagitaba::history::{
    EntryKind, ErrorClass, Event, History, HistoryError, HistoryStore, KeyName, MutationOperation,
    OperationId, Outcome, ProgramName, Query, Record, RunResult, Settings, SettingsUpdate,
    Termination,
};
use rusqlite::Connection;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const DB: &str = "history.sqlite3";
static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    store: HistoryStore,
    directory: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let nonce = FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let root = temp.join(format!("kagitaba-storage-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = root.join("history");
        let store = HistoryStore::new(directory.clone());
        Self {
            root,
            store,
            directory,
        }
    }

    fn enable(&self) {
        self.store
            .configure(SettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
    }

    fn connection(&self) -> Connection {
        Connection::open(self.directory.join(DB)).unwrap()
    }

    fn record(&self, event: Event) -> Record {
        let record = Record {
            operation_id: OperationId::new().unwrap(),
            event,
        };
        self.store.record(&record).unwrap();
        record
    }

    fn list(&self, failed: bool) -> Vec<kagitaba::history::Entry> {
        self.store
            .list(&Query {
                key: None,
                failed,
                limit: 100,
            })
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn key() -> KeyName {
    KeyName::new("SYNTHETIC_KEY").unwrap()
}

fn mutation(outcome: Outcome) -> Event {
    Event::Mutation {
        operation: MutationOperation::Create,
        key: Some(key()),
        outcome,
    }
}

fn run_start() -> Event {
    Event::RunStart {
        keys: vec![key()],
        program: Some(ProgramName::from_path("/synthetic/program").unwrap()),
    }
}

fn run_end(termination: Termination) -> Event {
    Event::RunEnd {
        keys: vec![key()],
        program: Some(ProgramName::from_path("/synthetic/program").unwrap()),
        termination,
    }
}

fn make_private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn disabled_default_and_configuration_persistence_do_not_touch_database() {
    let fixture = Fixture::new();
    assert_eq!(fixture.store.settings(), Ok(Settings::default()));
    fixture
        .store
        .record(&Record {
            operation_id: OperationId::new().unwrap(),
            event: mutation(Outcome::Success),
        })
        .unwrap();
    assert!(
        fixture
            .store
            .list(&Query {
                key: None,
                failed: false,
                limit: 10
            })
            .unwrap()
            .is_empty()
    );
    fixture.store.clear().unwrap();
    fixture.store.reclaim().unwrap();
    assert!(!fixture.directory.exists());

    fixture.enable();
    let db = fixture.directory.join(DB);
    make_private_file(&db, b"preserve-corrupt-database-bytes");
    let disabled = Settings {
        enabled: false,
        retention_days: 17,
        max_events: 300,
    };
    assert_eq!(
        fixture.store.configure(SettingsUpdate {
            enabled: Some(false),
            retention_days: Some(17),
            max_events: Some(300),
        }),
        Ok(disabled),
    );
    assert_eq!(
        HistoryStore::new(fixture.directory.clone()).settings(),
        Ok(disabled)
    );
    fixture
        .store
        .record(&Record {
            operation_id: OperationId::new().unwrap(),
            event: mutation(Outcome::Success),
        })
        .unwrap();
    assert_eq!(fs::read(db).unwrap(), b"preserve-corrupt-database-bytes");
    assert_eq!(
        fixture.store.list(&Query {
            key: None,
            failed: false,
            limit: 10
        }),
        Err(HistoryError::Database)
    );
}

#[test]
fn current_schema_initializes_and_unsupported_or_corrupt_databases_are_preserved() {
    let fixture = Fixture::new();
    fixture.enable();
    let record = fixture.record(mutation(Outcome::Success));
    assert_eq!(
        fixture
            .connection()
            .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        1
    );
    let reopened = HistoryStore::new(fixture.directory.clone());
    assert_eq!(
        reopened
            .list(&Query {
                key: None,
                failed: false,
                limit: 10
            })
            .unwrap()[0]
            .operation_id,
        record.operation_id
    );

    for schema in ["future", "foreign"] {
        let fixture = Fixture::new();
        fixture.enable();
        let db = fixture.directory.join(DB);
        make_private_file(&db, b"");
        let connection = Connection::open(&db).unwrap();
        if schema == "future" {
            connection.execute_batch("PRAGMA user_version=2; CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES ('preserve');").unwrap();
        } else {
            connection.execute_batch("CREATE TABLE unexpected(value TEXT); INSERT INTO unexpected VALUES ('preserve');").unwrap();
        }
        drop(connection);
        let before = fs::read(&db).unwrap();
        assert_eq!(
            fixture.store.list(&Query {
                key: None,
                failed: false,
                limit: 10
            }),
            Err(HistoryError::Database)
        );
        assert_eq!(fixture.store.clear(), Err(HistoryError::Database));
        assert_eq!(
            fs::read(db).unwrap(),
            before,
            "{schema} schema must remain byte-for-byte unchanged"
        );
    }

    let fixture = Fixture::new();
    fixture.enable();
    let db = fixture.directory.join(DB);
    make_private_file(&db, b"not-a-sqlite-database");
    let before = fs::read(&db).unwrap();
    assert_eq!(
        fixture.store.list(&Query {
            key: None,
            failed: false,
            limit: 10
        }),
        Err(HistoryError::Database)
    );
    assert_eq!(fs::read(db).unwrap(), before);
}

#[test]
fn symlinked_storage_and_sidecars_are_rejected_without_following_targets() {
    let fixture = Fixture::new();
    let target_dir = fixture.root.join("outside-directory");
    fs::create_dir(&target_dir).unwrap();
    fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let sentinel = target_dir.join("sentinel");
    fs::write(&sentinel, b"outside-target-untouched").unwrap();
    symlink(&target_dir, &fixture.directory).unwrap();
    assert_eq!(fixture.store.settings(), Err(HistoryError::UnsafePath));
    assert_eq!(fs::read(&sentinel).unwrap(), b"outside-target-untouched");

    let fixture = Fixture::new();
    fixture.enable();
    let target_db = fixture.root.join("outside-db");
    make_private_file(&target_db, b"database-target-untouched");
    symlink(&target_db, fixture.directory.join(DB)).unwrap();
    assert_eq!(
        fixture.store.list(&Query {
            key: None,
            failed: false,
            limit: 10
        }),
        Err(HistoryError::UnsafePath)
    );
    assert_eq!(fs::read(&target_db).unwrap(), b"database-target-untouched");

    for (name, call_list) in [
        ("config", false),
        ("history.sqlite3-journal", true),
        ("history.sqlite3-wal", true),
        ("history.sqlite3-shm", true),
    ] {
        let fixture = Fixture::new();
        fixture.enable();
        if call_list {
            fixture.record(mutation(Outcome::Success));
        }
        if name == "config" {
            fs::remove_file(fixture.directory.join(name)).unwrap();
        }
        let target = fixture.root.join(format!("external-{name}"));
        make_private_file(&target, b"related-target-untouched");
        symlink(&target, fixture.directory.join(name)).unwrap();
        let result = if call_list {
            fixture
                .store
                .list(&Query {
                    key: None,
                    failed: false,
                    limit: 10,
                })
                .map(|_| ())
        } else {
            fixture.store.settings().map(|_| ())
        };
        assert_eq!(result, Err(HistoryError::UnsafePath), "{name}");
        assert_eq!(
            fs::read(&target).unwrap(),
            b"related-target-untouched",
            "{name}"
        );
    }
}

#[test]
fn hardlinks_wrong_permissions_and_nonregular_children_fail_without_repair() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.record(mutation(Outcome::Success));
    let db = fixture.directory.join(DB);
    let outside = fixture.root.join("database-hardlink");
    fs::hard_link(&db, &outside).unwrap();
    assert_eq!(
        fixture.store.list(&Query {
            key: None,
            failed: false,
            limit: 10
        }),
        Err(HistoryError::UnsafePath)
    );
    assert_eq!(fs::metadata(&db).unwrap().nlink(), 2);

    for unsafe_child in ["wrong-mode", "directory"] {
        let fixture = Fixture::new();
        fixture.enable();
        let path = fixture.directory.join("unknown-child");
        if unsafe_child == "directory" {
            fs::create_dir(&path).unwrap();
        } else {
            make_private_file(&path, b"preserve");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(
            fixture.store.settings(),
            Err(HistoryError::UnsafePath),
            "{unsafe_child}"
        );
        if unsafe_child == "wrong-mode" {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
    }
}

#[test]
fn related_files_and_directory_are_private_and_parallel_writes_are_complete() {
    let fixture = Fixture::new();
    fixture.enable();
    let stores: Vec<_> = (0..4)
        .map(|_| HistoryStore::new(fixture.directory.clone()))
        .collect();
    let workers: Vec<_> = stores
        .into_iter()
        .map(|store| {
            std::thread::spawn(move || {
                store.record(&Record {
                    operation_id: OperationId::new().unwrap(),
                    event: mutation(Outcome::Success),
                })
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    assert_eq!(fixture.list(false).len(), 4);
    assert_eq!(
        fs::metadata(&fixture.directory)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for entry in fs::read_dir(&fixture.directory).unwrap() {
        let metadata = entry.unwrap().metadata().unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn sqlite_write_contention_is_bounded_and_does_not_drop_existing_records() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.record(mutation(Outcome::Success));
    let connection = fixture.connection();
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let started = Instant::now();
    let result = fixture.store.record(&Record {
        operation_id: OperationId::new().unwrap(),
        event: mutation(Outcome::Success),
    });
    let elapsed = started.elapsed();
    assert_eq!(result, Err(HistoryError::Busy));
    assert!(elapsed < Duration::from_secs(2));
    connection.execute_batch("ROLLBACK;").unwrap();
    assert_eq!(fixture.list(false).len(), 1);
}

#[test]
fn retention_and_filters_keep_groups_whole_and_classify_unpaired_runs() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture
        .store
        .configure(SettingsUpdate {
            max_events: Some(2),
            ..Default::default()
        })
        .unwrap();
    let pair = fixture.record(run_start());
    fixture
        .store
        .record(&Record {
            operation_id: pair.operation_id.clone(),
            event: run_end(Termination::Exited(0)),
        })
        .unwrap();
    fixture.record(mutation(Outcome::Success));
    let grouped: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE operation_id=?1",
            [pair.operation_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(grouped, 0, "count pruning must remove a run pair as a unit");

    fixture
        .store
        .configure(SettingsUpdate {
            max_events: Some(100),
            ..Default::default()
        })
        .unwrap();
    fixture.record(run_start());
    fixture.record(run_end(Termination::Exited(3)));
    let entries = fixture.list(false);
    assert!(entries.iter().any(|entry| matches!(
        entry.kind,
        EntryKind::Run {
            result: RunResult::Unknown,
            ..
        }
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry.kind,
        EntryKind::Run {
            result: RunResult::EndOnly(Termination::Exited(3)),
            ..
        }
    )));
    let failures = fixture
        .store
        .list(&Query {
            key: None,
            failed: true,
            limit: 100,
        })
        .unwrap();
    assert!(failures.iter().all(|entry| !matches!(
        entry.kind,
        EntryKind::Run {
            result: RunResult::Unknown,
            ..
        }
    )));
    assert!(failures.iter().any(|entry| matches!(
        entry.kind,
        EntryKind::Run {
            result: RunResult::EndOnly(Termination::Exited(3)),
            ..
        }
    )));
}

#[test]
fn clear_reclaims_old_rows_but_retains_configuration_and_corrupt_fields_are_not_echoed() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture
        .store
        .configure(SettingsUpdate {
            retention_days: Some(31),
            max_events: Some(1_000),
            ..Default::default()
        })
        .unwrap();
    for _ in 0..700 {
        fixture.record(mutation(Outcome::Success));
    }
    let settings_before = fixture.store.settings().unwrap();
    let before: u32 = fixture
        .connection()
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    fixture.store.clear().unwrap();
    let after: u32 = fixture
        .connection()
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    assert!(fixture.list(false).is_empty());
    assert!(after < before);
    assert_eq!(fixture.store.settings().unwrap(), settings_before);

    let private = "synthetic-raw-error-must-not-escape";
    fixture.record(Event::Mutation {
        operation: MutationOperation::Create,
        key: Some(key()),
        outcome: Outcome::Failure(ErrorClass::AccessDenied),
    });
    fixture
        .connection()
        .execute("UPDATE events SET error_class=?1", [private])
        .unwrap();
    let error = fixture
        .store
        .list(&Query {
            key: None,
            failed: false,
            limit: 10,
        })
        .unwrap_err();
    assert_eq!(error, HistoryError::Database);
    assert!(!error.to_string().contains(private));
}
