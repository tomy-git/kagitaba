// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::cli::HistoryConfigArgs;
use crate::history::HistoryStore;

fn event(f: &Fixture) -> Event {
    let state = f.history.0.borrow();
    assert_eq!(state.records.len(), 1);
    state.records[0].event.clone()
}

fn mutation(operation: MutationOperation, outcome: Outcome) -> Event {
    Event::Mutation {
        operation,
        key: KeyName::new(NAME),
        outcome,
    }
}

fn history(command: Option<HistoryCommand>) -> Command {
    Command::History(HistoryArgs {
        command,
        key: None,
        failed: false,
        limit: 50,
    })
}

#[test]
fn mutations_record_create_replace_and_delete_once() {
    let mut f = Fixture::default();
    f.run(set()).unwrap();
    assert_eq!(
        event(&f),
        mutation(MutationOperation::Create, Outcome::Success)
    );
    for concurrent in [false, true] {
        let mut f = Fixture::default();
        if concurrent {
            f.store.0.borrow_mut().concurrent_value = Some("concurrent-secret".into());
        } else {
            f.store.seed(NAME, "old-secret");
        }
        f.answer(true);
        f.run(set()).unwrap();
        assert_eq!(
            event(&f),
            mutation(MutationOperation::Replace, Outcome::Success)
        );
    }
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.answer(true);
    f.run(delete()).unwrap();
    assert_eq!(
        event(&f),
        mutation(MutationOperation::Delete, Outcome::Success)
    );
}

#[test]
fn mutation_failures_use_only_fixed_classes_and_preserve_cancellation() {
    for (store_operation, logged_operation, command) in [
        (Operation::Exists, MutationOperation::Set, set()),
        (Operation::Create, MutationOperation::Create, set()),
        (Operation::Delete, MutationOperation::Delete, delete()),
    ] {
        let mut f = Fixture::default();
        f.store
            .0
            .borrow_mut()
            .failures
            .insert(store_operation, StoreError::AccessDenied);
        if store_operation == Operation::Delete {
            f.answer(true);
        }
        assert!(f.run(command).is_err());
        assert_eq!(
            event(&f),
            mutation(logged_operation, Outcome::Failure(ErrorClass::AccessDenied))
        );
    }
    let mut f = Fixture::default();
    f.store
        .0
        .borrow_mut()
        .failures
        .insert(Operation::Create, StoreError::OperationCanceled);
    assert!(matches!(
        f.run(set()),
        Err(AppError::Store(StoreError::OperationCanceled))
    ));
    assert_eq!(
        event(&f),
        mutation(MutationOperation::Create, Outcome::Aborted)
    );
}

#[test]
fn refusals_prompt_io_and_missing_delete_are_distinct() {
    for command in [set(), delete()] {
        let mut f = Fixture::default();
        f.store.seed(NAME, "old-secret");
        f.answer(false);
        f.run(command).unwrap();
        assert!(matches!(
            event(&f),
            Event::Mutation {
                outcome: Outcome::Aborted,
                ..
            }
        ));
    }
    for confirm in [false, true] {
        let mut f = Fixture::default();
        if confirm {
            f.store.seed(NAME, "old-secret");
            f.prompt.0.borrow_mut().confirm_error = Some(io::ErrorKind::BrokenPipe);
        } else {
            f.prompt.0.borrow_mut().secret_error = Some(io::ErrorKind::BrokenPipe);
        }
        assert!(f.run(set()).is_err());
        assert!(matches!(
            event(&f),
            Event::Mutation {
                outcome: Outcome::Failure(ErrorClass::PromptIo),
                ..
            }
        ));
    }
    let mut f = Fixture::default();
    f.answer(true);
    assert_eq!(f.run(delete()).unwrap(), 0);
    assert_eq!(
        event(&f),
        mutation(
            MutationOperation::Delete,
            Outcome::Failure(ErrorClass::NotFound)
        )
    );
}

#[test]
fn invalid_input_is_never_retained_as_a_key_or_error_text() {
    let sentinel = "private-invalid-key\nvalue";
    for command in [
        Command::Set(SetArgs {
            env_name: sentinel.into(),
        }),
        Command::Delete(DeleteArgs {
            env_name: sentinel.into(),
        }),
        run(&[sentinel]),
    ] {
        let mut f = Fixture::default();
        let error = f.run(command).unwrap_err();
        assert!(!error.to_string().contains(sentinel));
        match event(&f) {
            Event::Mutation {
                key: None,
                outcome: Outcome::Failure(ErrorClass::InvalidInput),
                ..
            } => {}
            Event::RunFailure {
                keys,
                error: ErrorClass::InvalidInput,
                ..
            } => assert!(keys.is_empty()),
            _ => panic!("unexpected invalid input history"),
        }
        assert!(f.calls().is_empty());
    }
}

#[test]
fn stdout_failure_does_not_reclassify_completed_mutation_or_refusal() {
    for (command, expected) in [
        (set(), mutation(MutationOperation::Create, Outcome::Success)),
        (
            delete(),
            mutation(MutationOperation::Delete, Outcome::Aborted),
        ),
    ] {
        let mut f = Fixture::default();
        f.answer(false);
        assert!(matches!(
            f.app.run(
                Cli { command },
                &mut FailedOutput { fail_flush: false },
                &mut f.err
            ),
            Err(AppError::Io(_))
        ));
        assert_eq!(event(&f), expected);
    }
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.answer(true);
    assert!(matches!(
        f.app.run(
            Cli { command: delete() },
            &mut FailedOutput { fail_flush: false },
            &mut f.err
        ),
        Err(AppError::Io(_))
    ));
    assert_eq!(
        event(&f),
        mutation(MutationOperation::Delete, Outcome::Success)
    );
}

#[test]
fn run_records_start_and_end_with_correlated_id_and_basename_only() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.runner.0.borrow_mut().exit_code = 143;
    let command = Command::Run(RunArgs {
        keys: vec![NAME.into()],
        command: vec!["/private/path/program".into(), "private-argument".into()],
    });
    assert_eq!(f.run(command).unwrap(), 143);
    let records = &f.history.0.borrow().records;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].operation_id, records[1].operation_id);
    assert_eq!(
        records[0].event,
        Event::RunStart {
            keys: vec![KeyName::new(NAME).unwrap()],
            program: ProgramName::from_path("program")
        }
    );
    assert!(matches!(
        records[1].event,
        Event::RunEnd {
            termination: Termination::Exited(143),
            ..
        }
    ));
    let metadata = format!("{records:?}");
    for sentinel in ["old-secret", "private-argument", "/private/path"] {
        assert!(!metadata.contains(sentinel));
    }
}

#[test]
fn run_signal_exit_and_unknown_wait_have_distinct_events() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.runner.0.borrow_mut().signal = Some(15);
    assert_eq!(f.run(run(&[NAME])).unwrap(), 143);
    assert!(matches!(
        f.history.0.borrow().records[1].event,
        Event::RunEnd {
            termination: Termination::Signaled(15),
            ..
        }
    ));
    for wait_failure in [false, true] {
        let mut f = Fixture::default();
        f.store.seed(NAME, "old-secret");
        f.runner.0.borrow_mut().wait_failure = wait_failure;
        f.runner.0.borrow_mut().unknown = !wait_failure;
        let result = f.run(run(&[NAME]));
        if wait_failure {
            assert!(matches!(
                result,
                Err(AppError::Process(ProcessError::Wait(_)))
            ));
        } else {
            assert_eq!(result.unwrap(), 1);
        }
        let state = f.history.0.borrow();
        assert_eq!(state.records.len(), 2);
        assert!(matches!(
            state.records[1].event,
            Event::RunEnd {
                termination: Termination::Unknown,
                ..
            }
        ));
    }
}

#[test]
fn get_and_spawn_failure_do_not_create_a_start() {
    let mut f = Fixture::default();
    assert!(f.run(run(&[NAME])).is_err());
    assert!(matches!(
        event(&f),
        Event::RunFailure {
            error: ErrorClass::NotFound,
            ..
        }
    ));
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.runner.0.borrow_mut().failure = Some(io::ErrorKind::NotFound);
    assert!(f.run(run(&[NAME])).is_err());
    assert!(matches!(
        event(&f),
        Event::RunFailure {
            error: ErrorClass::Launch,
            ..
        }
    ));
}

#[test]
fn history_failures_never_change_key_operations_or_child_exit() {
    for history_error in [
        HistoryError::Unavailable,
        HistoryError::Busy,
        HistoryError::Database,
        HistoryError::UnsafePath,
        HistoryError::Configuration,
    ] {
        let mut f = Fixture::default();
        f.history.0.borrow_mut().failure = Some(history_error);
        assert_eq!(f.run(set()).unwrap(), 0);
        assert!(f.store.0.borrow().values.contains_key(NAME));
        assert_eq!(f.err, format!("{HISTORY_WARNING}\n").as_bytes());
        f.out.clear();
        f.err.clear();
        f.runner.0.borrow_mut().exit_code = 42;
        assert_eq!(f.run(run(&[NAME])).unwrap(), 42);
        assert_eq!(
            f.err,
            format!("{HISTORY_WARNING}\n{HISTORY_WARNING}\n").as_bytes()
        );
        f.assert_no_secret_output();
    }
}

#[test]
fn status_never_accesses_history_even_when_broken() {
    let mut f = Fixture::default();
    f.history.0.borrow_mut().failure = Some(HistoryError::Unavailable);
    f.run(Command::Status(StatusArgs { env_name: None }))
        .unwrap();
    f.run(Command::Status(StatusArgs {
        env_name: Some(NAME.into()),
    }))
    .unwrap();
    assert!(f.history.0.borrow().calls.is_empty());
    assert!(f.err.is_empty());
}

#[test]
fn management_never_calls_keychain_and_clear_requires_confirmation() {
    for command in [
        history(None),
        history(Some(HistoryCommand::Enable)),
        history(Some(HistoryCommand::Disable)),
        history(Some(HistoryCommand::Config(HistoryConfigArgs {
            retention_days: None,
            max_events: None,
        }))),
        history(Some(HistoryCommand::Config(HistoryConfigArgs {
            retention_days: Some(30),
            max_events: Some(100),
        }))),
        history(Some(HistoryCommand::Reclaim)),
    ] {
        let mut f = Fixture::default();
        f.run(command).unwrap();
        assert!(f.calls().is_empty());
        assert!(f.history.0.borrow().records.is_empty());
    }
    for answer in [false, true] {
        let mut f = Fixture::default();
        f.answer(answer);
        f.run(history(Some(HistoryCommand::Clear))).unwrap();
        assert!(f.calls().is_empty());
        assert_eq!(
            f.history.0.borrow().calls,
            if answer { vec!["clear"] } else { vec![] }
        );
        assert_eq!(
            f.out,
            if answer {
                b"History cleared.\n".as_slice()
            } else {
                b"Aborted.\n".as_slice()
            }
        );
    }
}

#[test]
fn history_filter_is_validated_without_echoing_invalid_value() {
    let mut f = Fixture::default();
    let bad = "private-filter-secret";
    let error = f
        .run(Command::History(HistoryArgs {
            command: None,
            key: Some(bad.into()),
            failed: false,
            limit: 50,
        }))
        .unwrap_err();
    assert!(!error.to_string().contains(bad));
    assert!(f.history.0.borrow().calls.is_empty());
    f.run(Command::History(HistoryArgs {
        command: None,
        key: Some(NAME.into()),
        failed: true,
        limit: 3,
    }))
    .unwrap();
    let state = f.history.0.borrow();
    let query = state.query.as_ref().unwrap();
    assert_eq!(query.key, KeyName::new(NAME));
    assert!(query.failed);
    assert_eq!(query.limit, 3);
}

#[test]
fn unknown_and_end_only_display_do_not_invent_a_success() {
    let mut f = Fixture::default();
    f.history.0.borrow_mut().entries = [
        RunResult::Unknown,
        RunResult::EndOnly(Termination::Exited(0)),
    ]
    .into_iter()
    .map(|result| Entry {
        utc_ms: 123,
        operation_id: OperationId::new().unwrap(),
        kind: EntryKind::Run {
            keys: vec![KeyName::new(NAME).unwrap()],
            program: ProgramName::from_path("program"),
            result,
        },
    })
    .collect();
    f.run(history(None)).unwrap();
    let output = String::from_utf8(f.out).unwrap();
    assert!(output.contains("1970-01-01T00:00:00.123Z"));
    assert!(output.contains("unknown (no end recorded)"));
    assert!(output.contains("end-only exit=0"));
    assert!(!output.contains("success"));
}

#[test]
fn logical_clear_failure_reports_reclamation_specifically() {
    let mut f = Fixture::default();
    f.history.0.borrow_mut().failure = Some(HistoryError::Reclaim);
    f.answer(true);
    let error = f.run(history(Some(HistoryCommand::Clear))).unwrap_err();
    assert!(matches!(error, AppError::History(HistoryError::Reclaim)));
    assert!(error.to_string().contains("cleared"));
    assert!(error.to_string().contains("reclamation failed"));
    assert!(f.out.is_empty());
}

#[test]
fn long_existing_key_names_continue_working_but_are_omitted_from_history() {
    let key = "A".repeat(257);
    let mut f = Fixture::default();
    assert_eq!(
        f.run(Command::Set(SetArgs {
            env_name: key.clone()
        }))
        .unwrap(),
        0
    );
    assert!(matches!(
        event(&f),
        Event::Mutation {
            key: None,
            outcome: Outcome::Success,
            ..
        }
    ));
    f.history.0.borrow_mut().records.clear();
    f.run(run(&[&key])).unwrap();
    assert!(
        matches!(&f.history.0.borrow().records[0].event, Event::RunStart { keys, .. } if keys.is_empty())
    );
}

#[test]
fn real_history_database_and_related_files_exclude_secret_argument_and_child_output() {
    let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "kagitaba-app-history-{}",
        OperationId::new().unwrap().as_str()
    ));
    let store = HistoryStore::new(directory.clone());
    store
        .configure(SettingsUpdate {
            enabled: Some(true),
            ..SettingsUpdate::default()
        })
        .unwrap();
    let mut f = Fixture::default();
    f.store.seed(NAME, "private-value-sentinel");
    f.app = App::new(
        Box::new(f.store.clone()),
        Box::new(f.prompt.clone()),
        Box::new(crate::process::SystemCommandRunner),
    )
    .with_history(Box::new(HistoryStore::new(directory.clone())));
    let output_path = directory.with_extension("child-output");
    let child_script = "exec 2>> \"$2\"; test \"$TEST_KEY\" = private-value-sentinel || exit 2; test \"$1\" = private-arg-sentinel || exit 3; printf private-child-output-sentinel > \"$2\"; printf private-child-error-sentinel >&2";
    f.run(Command::Run(RunArgs {
        keys: vec![NAME.into()],
        command: vec![
            "/bin/sh".into(),
            "-c".into(),
            child_script.into(),
            "fixture".into(),
            "private-arg-sentinel".into(),
            output_path.to_str().unwrap().into(),
        ],
    }))
    .unwrap();
    assert_eq!(
        std::fs::read(&output_path).unwrap(),
        b"private-child-output-sentinelprivate-child-error-sentinel"
    );
    f.run(history(None)).unwrap();
    let sentinels: &[&[u8]] = &[
        b"private-value-sentinel",
        b"private-arg-sentinel",
        b"private-directory-sentinel",
        b"private-child-output-sentinel",
        b"private-child-error-sentinel",
        b"private-backend-error",
    ];
    for bytes in [f.out.as_slice(), f.err.as_slice()] {
        for sentinel in sentinels {
            assert!(
                !bytes
                    .windows(sentinel.len())
                    .any(|window| window == *sentinel)
            );
        }
    }
    for file in std::fs::read_dir(&directory).unwrap() {
        let path = file.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(path).unwrap();
            for sentinel in sentinels {
                assert!(
                    !bytes
                        .windows(sentinel.len())
                        .any(|window| window == *sentinel)
                );
            }
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
    std::fs::remove_file(output_path).unwrap();
}

#[test]
fn disabled_history_is_inert_for_normal_key_operations() {
    let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "kagitaba-app-disabled-{}",
        OperationId::new().unwrap().as_str()
    ));
    let mut f = Fixture::default();
    f.app = App::new(
        Box::new(f.store.clone()),
        Box::new(f.prompt.clone()),
        Box::new(f.runner.clone()),
    )
    .with_history(Box::new(HistoryStore::new(directory.clone())));
    f.run(set()).unwrap();
    f.run(run(&[NAME])).unwrap();
    assert!(!directory.exists());
    assert!(f.err.is_empty());
}

#[test]
fn clear_prompt_failure_does_not_touch_history_or_keychain() {
    let mut f = Fixture::default();
    f.prompt.0.borrow_mut().confirm_error = Some(io::ErrorKind::BrokenPipe);
    assert!(matches!(
        f.run(history(Some(HistoryCommand::Clear))),
        Err(AppError::Io(_))
    ));
    assert!(f.history.0.borrow().calls.is_empty());
    assert!(f.calls().is_empty());
}

#[test]
fn launch_diagnostics_and_history_exclude_raw_program_path_and_error() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.runner.0.borrow_mut().failure = Some(io::ErrorKind::NotFound);
    let error = f
        .run(Command::Run(RunArgs {
            keys: vec![NAME.into()],
            command: vec![
                "/private-path-sentinel/program".into(),
                "private-argument-sentinel".into(),
            ],
        }))
        .unwrap_err();
    let recorded = format!("{:?}", f.history.0.borrow().records);
    for sentinel in [
        "private-path-sentinel",
        "private-argument-sentinel",
        "private-backend-error",
        "old-secret",
    ] {
        assert!(!recorded.contains(sentinel));
        assert!(!String::from_utf8_lossy(&f.err).contains(sentinel));
        assert!(!error.to_string().contains(sentinel));
    }
    assert_eq!(f.err, b"failed to launch child process\n");
}
