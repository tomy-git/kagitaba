// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    store: HistoryStore,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kagitaba-history-test-{}",
            OperationId::new().unwrap().as_str()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = HistoryStore::new(root.join("private"));
        Self { root, store }
    }
    fn path(&self) -> &std::path::Path {
        self.store.directory.as_deref().unwrap()
    }
    fn enable(&self) {
        self.store
            .configure(SettingsUpdate {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
    }
    fn connection(&self) -> rusqlite::Connection {
        let path = self.path().join(db::FILE);
        if !path.exists() {
            precreate(&path, b"");
        }
        rusqlite::Connection::open(path).unwrap()
    }
    fn event(&self, event: Event) -> Record {
        let record = Record {
            operation_id: OperationId::new().unwrap(),
            event,
        };
        self.store.record(&record).unwrap();
        record
    }
    fn list(&self) -> Vec<Entry> {
        self.store.list(&query()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn query() -> Query {
    Query {
        key: None,
        failed: false,
        limit: 50,
    }
}
fn key() -> KeyName {
    KeyName::new("API_KEY").unwrap()
}
fn mutation(outcome: Outcome) -> Event {
    Event::Mutation {
        operation: MutationOperation::Create,
        key: Some(key()),
        outcome,
    }
}
fn start() -> Event {
    Event::RunStart {
        keys: vec![key()],
        program: ProgramName::from_path("/bin/tool"),
    }
}
fn end(termination: Termination) -> Event {
    Event::RunEnd {
        keys: vec![key()],
        program: ProgramName::from_path("/bin/tool"),
        termination,
    }
}
fn precreate(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    file.write_all(bytes).unwrap();
}

#[test]
fn disabled_defaults_do_not_create_directory_or_database() {
    let fixture = Fixture::new();
    assert_eq!(fixture.store.settings(), Ok(Settings::default()));
    fixture.event(mutation(Outcome::Success));
    assert!(fixture.list().is_empty());
    fixture.store.clear().unwrap();
    fixture.store.reclaim().unwrap();
    assert!(!fixture.path().exists());
}

#[test]
fn absent_home_is_fixed_unavailable() {
    let store = HistoryStore { directory: None };
    assert_eq!(store.settings(), Err(HistoryError::Unavailable));
    assert_eq!(
        store.configure(SettingsUpdate::default()),
        Err(HistoryError::Unavailable)
    );
    assert_eq!(store.list(&query()), Err(HistoryError::Unavailable));
}

#[test]
fn config_persists_separately_and_disable_does_not_open_corrupt_database() {
    let fixture = Fixture::new();
    fixture.enable();
    assert!(!fixture.path().join(db::FILE).exists());
    precreate(
        &fixture.path().join(db::FILE),
        b"corrupt-database-preserved",
    );
    let expected = Settings {
        enabled: false,
        retention_days: 17,
        max_events: 300,
    };
    assert_eq!(
        fixture.store.configure(SettingsUpdate {
            enabled: Some(false),
            retention_days: Some(17),
            max_events: Some(300)
        }),
        Ok(expected)
    );
    assert_eq!(
        HistoryStore::new(fixture.path().into()).settings(),
        Ok(expected)
    );
    fixture.event(mutation(Outcome::Success));
    assert_eq!(
        fs::read(fixture.path().join(db::FILE)).unwrap(),
        b"corrupt-database-preserved"
    );
    assert_eq!(fixture.store.list(&query()), Err(HistoryError::Database));
}

#[test]
fn invalid_settings_do_not_replace_previous_configuration() {
    let fixture = Fixture::new();
    fixture.enable();
    let before = fs::read(fixture.path().join(config::FILE)).unwrap();
    for update in [
        SettingsUpdate {
            retention_days: Some(0),
            ..Default::default()
        },
        SettingsUpdate {
            retention_days: Some(3651),
            ..Default::default()
        },
        SettingsUpdate {
            max_events: Some(1),
            ..Default::default()
        },
        SettingsUpdate {
            max_events: Some(1_000_001),
            ..Default::default()
        },
    ] {
        assert_eq!(
            fixture.store.configure(update),
            Err(HistoryError::Configuration)
        );
        assert_eq!(fs::read(fixture.path().join(config::FILE)).unwrap(), before);
    }
}

#[test]
fn malformed_or_oversized_config_is_never_silently_reset() {
    for content in [
        b"enabled=maybe\n".to_vec(),
        b"enabled=true\nenabled=false\nretention_days=90\nmax_events=10000\n".to_vec(),
        vec![b'x'; 1025],
    ] {
        let fixture = Fixture::new();
        fixture.enable();
        fs::write(fixture.path().join(config::FILE), &content).unwrap();
        assert_eq!(fixture.store.settings(), Err(HistoryError::Configuration));
        assert_eq!(
            fixture.store.configure(SettingsUpdate {
                enabled: Some(false),
                ..Default::default()
            }),
            Err(HistoryError::Configuration)
        );
        assert_eq!(
            fs::read(fixture.path().join(config::FILE)).unwrap(),
            content
        );
    }
}

#[test]
fn config_lock_wait_is_bounded_and_does_not_lose_an_update() {
    let fixture = Fixture::new();
    fixture.enable();
    let directory = fixture.store.directory(false).unwrap().unwrap();
    let lock = config::lock(&directory).unwrap();
    let now = Instant::now();
    assert_eq!(
        fixture.store.configure(SettingsUpdate {
            retention_days: Some(10),
            ..Default::default()
        }),
        Err(HistoryError::Busy)
    );
    assert!(now.elapsed() >= Duration::from_millis(200));
    assert!(now.elapsed() < Duration::from_secs(3));
    drop(lock);
    fixture
        .store
        .configure(SettingsUpdate {
            max_events: Some(200),
            ..Default::default()
        })
        .unwrap();
    fixture
        .store
        .configure(SettingsUpdate {
            retention_days: Some(10),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        fixture.store.settings().unwrap(),
        Settings {
            enabled: true,
            retention_days: 10,
            max_events: 200
        }
    );
}

#[test]
fn concurrent_config_read_modify_write_retains_both_independent_updates() {
    let fixture = Fixture::new();
    fixture.enable();
    let one = HistoryStore::new(fixture.path().into());
    let two = HistoryStore::new(fixture.path().into());
    let first = std::thread::spawn(move || {
        one.configure(SettingsUpdate {
            retention_days: Some(42),
            ..Default::default()
        })
    });
    let second = std::thread::spawn(move || {
        two.configure(SettingsUpdate {
            max_events: Some(400),
            ..Default::default()
        })
    });
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert_eq!(
        fixture.store.settings().unwrap(),
        Settings {
            enabled: true,
            retention_days: 42,
            max_events: 400
        }
    );
}

#[test]
fn history_files_remain_private_including_a_live_journal() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.event(mutation(Outcome::Success));
    let connection = fixture.connection();
    connection
        .execute_batch("BEGIN IMMEDIATE; UPDATE events SET utc_ms=utc_ms+1;")
        .unwrap();
    assert!(fixture.path().join("history.sqlite3-journal").exists());
    assert_eq!(
        fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for child in fs::read_dir(fixture.path()).unwrap() {
        assert_eq!(
            child.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    connection.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn unsafe_directory_or_ancestor_permissions_are_rejected_without_repair() {
    let fixture = Fixture::new();
    fixture.enable();
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(fixture.store.settings(), Err(HistoryError::UnsafePath));
    assert_eq!(
        fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(fixture.store.settings(), Err(HistoryError::UnsafePath));
}

#[test]
fn symlinked_directory_and_database_are_rejected() {
    let fixture = Fixture::new();
    let target = fixture.root.join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, fixture.path()).unwrap();
    assert_eq!(fixture.store.settings(), Err(HistoryError::UnsafePath));
    fs::remove_file(fixture.path()).unwrap();
    fixture.enable();
    let target = fixture.root.join("outside-db");
    precreate(&target, b"keep");
    symlink(&target, fixture.path().join(db::FILE)).unwrap();
    assert_eq!(fixture.store.list(&query()), Err(HistoryError::UnsafePath));
    assert_eq!(fs::read(target).unwrap(), b"keep");
}

#[test]
fn hard_links_nonregular_files_and_unsafe_backups_are_rejected() {
    for kind in ["hardlink", "directory", "mode"] {
        let fixture = Fixture::new();
        fixture.enable();
        let file = fixture.path().join("unknown-backup");
        match kind {
            "hardlink" => {
                let original = fixture.root.join("original");
                precreate(&original, b"keep");
                fs::hard_link(original, file).unwrap();
            }
            "directory" => fs::create_dir(file).unwrap(),
            _ => {
                precreate(&file, b"keep");
                fs::set_permissions(file, fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        assert_eq!(fixture.store.settings(), Err(HistoryError::UnsafePath));
    }
}

#[test]
fn unsafe_sidecars_are_rejected_before_sqlite_can_use_them() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let fixture = Fixture::new();
        fixture.enable();
        fixture.event(mutation(Outcome::Success));
        let outside = fixture.root.join("outside");
        precreate(&outside, b"keep");
        symlink(
            &outside,
            fixture.path().join(format!("{}{suffix}", db::FILE)),
        )
        .unwrap();
        assert_eq!(fixture.store.list(&query()), Err(HistoryError::UnsafePath));
        assert_eq!(fs::read(outside).unwrap(), b"keep");
    }
}

#[test]
fn schema_zero_initializes_and_current_schema_reopens_with_persistence() {
    let fixture = Fixture::new();
    fixture.enable();
    precreate(&fixture.path().join(db::FILE), b"");
    let record = fixture.event(mutation(Outcome::Success));
    assert_eq!(
        fixture
            .connection()
            .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
            .unwrap(),
        1
    );
    let entries = HistoryStore::new(fixture.path().into())
        .list(&query())
        .unwrap();
    assert_eq!(entries[0].operation_id, record.operation_id);
}

#[test]
fn future_schema_foreign_tables_and_corruption_are_preserved() {
    for kind in ["future", "foreign", "corrupt"] {
        let fixture = Fixture::new();
        fixture.enable();
        precreate(&fixture.path().join(db::FILE), b"");
        if kind == "corrupt" {
            fs::write(fixture.path().join(db::FILE), b"not-a-database-preserve").unwrap();
        } else {
            fixture.connection().execute_batch(if kind == "future" { "PRAGMA user_version=2; CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES ('keep');" } else { "CREATE TABLE unexpected(value TEXT); INSERT INTO unexpected VALUES ('keep');" }).unwrap();
        }
        let before = fs::read(fixture.path().join(db::FILE)).unwrap();
        assert_eq!(fixture.store.list(&query()), Err(HistoryError::Database));
        assert_eq!(fixture.store.clear(), Err(HistoryError::Database));
        assert_eq!(fs::read(fixture.path().join(db::FILE)).unwrap(), before);
    }
}

#[test]
fn current_schema_with_extra_objects_is_rejected() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.event(mutation(Outcome::Success));
    fixture
        .connection()
        .execute_batch("CREATE TRIGGER unknown AFTER INSERT ON events BEGIN SELECT 1; END;")
        .unwrap();
    assert_eq!(fixture.store.list(&query()), Err(HistoryError::Database));
}

#[test]
fn database_contention_is_bounded_and_keeps_existing_records() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.event(mutation(Outcome::Success));
    let connection = fixture.connection();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let now = Instant::now();
    assert_eq!(
        fixture.store.record(&Record {
            operation_id: OperationId::new().unwrap(),
            event: mutation(Outcome::Success)
        }),
        Err(HistoryError::Busy)
    );
    assert!(now.elapsed() >= Duration::from_millis(200));
    assert!(now.elapsed() < Duration::from_secs(3));
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(fixture.list().len(), 1);
}

#[test]
fn concurrent_connections_initialize_and_preserve_each_record() {
    let fixture = Fixture::new();
    fixture.enable();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let store = HistoryStore::new(fixture.path().into());
            std::thread::spawn(move || {
                store.record(&Record {
                    operation_id: OperationId::new().unwrap(),
                    event: mutation(Outcome::Success),
                })
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    assert_eq!(fixture.list().len(), 4);
}

#[test]
fn run_grouping_reports_unknown_paired_and_end_only_results() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.event(start());
    let paired = fixture.event(start());
    fixture
        .store
        .record(&Record {
            operation_id: paired.operation_id,
            event: end(Termination::Exited(0)),
        })
        .unwrap();
    fixture.event(end(Termination::Signaled(15)));
    let entries = fixture.list();
    assert_eq!(entries.len(), 3);
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
            result: RunResult::Finished(Termination::Exited(0)),
            ..
        }
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry.kind,
        EntryKind::Run {
            result: RunResult::EndOnly(Termination::Signaled(15)),
            ..
        }
    )));
}

#[test]
fn failed_filter_includes_only_terminal_failures_not_unknown_or_aborted() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.event(start());
    fixture.event(end(Termination::Unknown));
    fixture.event(end(Termination::Exited(0)));
    fixture.event(mutation(Outcome::Success));
    fixture.event(mutation(Outcome::Aborted));
    fixture.event(mutation(Outcome::Failure(ErrorClass::AccessDenied)));
    fixture.event(Event::RunFailure {
        keys: vec![key()],
        program: None,
        error: ErrorClass::Launch,
    });
    fixture.event(end(Termination::Exited(3)));
    fixture.event(end(Termination::Signaled(15)));
    let entries = fixture
        .store
        .list(&Query {
            failed: true,
            ..query()
        })
        .unwrap();
    assert_eq!(entries.len(), 4);
}

#[test]
fn key_filter_and_limit_apply_to_complete_groups() {
    let fixture = Fixture::new();
    fixture.enable();
    let record = fixture.event(start());
    fixture
        .store
        .record(&Record {
            operation_id: record.operation_id,
            event: end(Termination::Exited(1)),
        })
        .unwrap();
    fixture.event(Event::Mutation {
        operation: MutationOperation::Delete,
        key: KeyName::new("OTHER_KEY"),
        outcome: Outcome::Success,
    });
    let entries = fixture
        .store
        .list(&Query {
            key: Some(key()),
            limit: 1,
            failed: true,
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(matches!(
        entries[0].kind,
        EntryKind::Run {
            result: RunResult::Finished(Termination::Exited(1)),
            ..
        }
    ));
    for limit in [0, 1001] {
        assert_eq!(
            fixture.store.list(&Query { limit, ..query() }),
            Err(HistoryError::Configuration)
        );
    }
}

#[test]
fn count_pruning_never_splits_a_run_pair() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture
        .store
        .configure(SettingsUpdate {
            max_events: Some(2),
            ..Default::default()
        })
        .unwrap();
    let pair = fixture.event(start());
    fixture
        .store
        .record(&Record {
            operation_id: pair.operation_id.clone(),
            event: end(Termination::Exited(0)),
        })
        .unwrap();
    fixture.event(mutation(Outcome::Success));
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE operation_id=?1",
                [pair.operation_id.as_str()],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(fixture.list().len(), 1);
}

#[test]
fn age_pruning_expires_a_whole_group_by_its_newest_event() {
    let fixture = Fixture::new();
    fixture.enable();
    let old = fixture.event(start());
    fixture
        .store
        .record(&Record {
            operation_id: old.operation_id.clone(),
            event: end(Termination::Exited(0)),
        })
        .unwrap();
    fixture
        .connection()
        .execute(
            "UPDATE events SET utc_ms=0 WHERE operation_id=?1",
            [old.operation_id.as_str()],
        )
        .unwrap();
    let current = fixture.event(start());
    fixture
        .connection()
        .execute(
            "UPDATE events SET utc_ms=0 WHERE operation_id=?1 AND kind='run_start'",
            [current.operation_id.as_str()],
        )
        .unwrap();
    fixture
        .store
        .record(&Record {
            operation_id: current.operation_id.clone(),
            event: end(Termination::Exited(0)),
        })
        .unwrap();
    assert_eq!(fixture.list().len(), 1);
    assert_eq!(fixture.list()[0].operation_id, current.operation_id);
}

#[test]
fn clear_then_late_end_remains_end_only_and_disabled_can_still_list_clear() {
    let fixture = Fixture::new();
    fixture.enable();
    let pending = fixture.event(start());
    fixture.store.clear().unwrap();
    fixture
        .store
        .record(&Record {
            operation_id: pending.operation_id,
            event: end(Termination::Exited(0)),
        })
        .unwrap();
    assert!(matches!(
        fixture.list()[0].kind,
        EntryKind::Run {
            result: RunResult::EndOnly(_),
            ..
        }
    ));
    fixture
        .store
        .configure(SettingsUpdate {
            enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
    fixture.event(mutation(Outcome::Success));
    assert_eq!(fixture.list().len(), 1);
    fixture.store.clear().unwrap();
    fixture.store.reclaim().unwrap();
    assert!(fixture.list().is_empty());
}

#[test]
fn clear_reclaims_pages_and_keeps_private_permissions() {
    let fixture = Fixture::new();
    fixture.enable();
    let connection = fixture.connection();
    for _ in 0..60 {
        fixture.event(mutation(Outcome::Success));
    }
    let before: u32 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    fixture.store.clear().unwrap();
    let after: u32 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    assert!(after < before);
    assert!(fixture.list().is_empty());
    assert_eq!(
        fs::metadata(fixture.path().join(db::FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn corrupt_payloads_do_not_emit_unapproved_information() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.event(mutation(Outcome::Success));
    fixture
        .connection()
        .execute(
            "UPDATE events SET error_class='raw-private-error', outcome='failure'",
            [],
        )
        .unwrap();
    assert_eq!(fixture.store.list(&query()), Err(HistoryError::Database));
    assert_eq!(
        HistoryError::Database.to_string(),
        "history database is corrupt or has an unsupported schema"
    );
}

#[test]
fn list_applies_age_and_new_configuration_count_limits_without_new_records() {
    let fixture = Fixture::new();
    fixture.enable();
    let old = fixture.event(mutation(Outcome::Success));
    fixture
        .connection()
        .execute(
            "UPDATE events SET utc_ms=0 WHERE operation_id=?1",
            [old.operation_id.as_str()],
        )
        .unwrap();
    assert!(fixture.list().is_empty());
    for _ in 0..5 {
        fixture.event(mutation(Outcome::Success));
    }
    fixture
        .store
        .configure(SettingsUpdate {
            enabled: Some(false),
            max_events: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(fixture.list().len(), 2);
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, u32>(0))
            .unwrap(),
        2
    );
}

#[test]
fn sqlite_full_rolls_back_new_events_without_deleting_existing_history() {
    let fixture = Fixture::new();
    fixture.enable();
    fixture.event(mutation(Outcome::Success));
    let directory = fixture.store.directory(false).unwrap().unwrap();
    let mut database = db::Database::open(&directory, false).unwrap().unwrap();
    let pages: u32 = database
        .connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    database
        .connection
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    let before = fs::read(fixture.path().join(db::FILE)).unwrap();
    let record = Record {
        operation_id: OperationId::new().unwrap(),
        event: Event::RunStart {
            keys: (0..1000)
                .map(|i| KeyName::new(&format!("LONG_KEY_{i}_{}", "X".repeat(200))).unwrap())
                .collect(),
            program: None,
        },
    };
    assert_eq!(
        db::record(
            &mut database.connection,
            &record,
            Settings {
                enabled: true,
                ..Default::default()
            },
            utc_ms().unwrap()
        ),
        Err(HistoryError::Database)
    );
    assert_eq!(fs::read(fixture.path().join(db::FILE)).unwrap(), before);
    drop(database);
    assert_eq!(fixture.list().len(), 1);
}

#[test]
fn pruning_an_old_start_then_recording_its_late_end_reports_end_only() {
    let fixture = Fixture::new();
    fixture.enable();
    let pending = fixture.event(start());
    fixture
        .connection()
        .execute("UPDATE events SET utc_ms=0", [])
        .unwrap();
    assert!(fixture.list().is_empty());
    fixture
        .store
        .record(&Record {
            operation_id: pending.operation_id,
            event: end(Termination::Exited(0)),
        })
        .unwrap();
    assert!(matches!(
        fixture.list()[0].kind,
        EntryKind::Run {
            result: RunResult::EndOnly(_),
            ..
        }
    ));
}

#[test]
fn failed_filter_precedes_limit_even_when_newest_groups_are_unknown() {
    let fixture = Fixture::new();
    fixture.enable();
    let failed = fixture.event(mutation(Outcome::Failure(ErrorClass::Backend)));
    for _ in 0..5 {
        fixture.event(start());
    }
    let entries = fixture
        .store
        .list(&Query {
            failed: true,
            limit: 1,
            ..query()
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].operation_id, failed.operation_id);
}
