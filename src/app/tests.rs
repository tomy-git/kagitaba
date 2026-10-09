// SPDX-License-Identifier: MPL-2.0

use super::*;

#[path = "test_support.rs"]
mod support;
use support::{Fixture, Operation};

const NAME: &str = "TEST_KEY";

fn set() -> Command {
    Command::Set(SetArgs {
        env_name: NAME.into(),
    })
}
fn delete() -> Command {
    Command::Delete(DeleteArgs {
        env_name: NAME.into(),
    })
}
fn run(keys: &[&str]) -> Command {
    Command::Run(RunArgs {
        keys: keys.iter().map(|key| (*key).into()).collect(),
        command: vec!["program".into(), "literal argument".into()],
    })
}
fn failures() -> [StoreError; 4] {
    [
        StoreError::AccessDenied,
        StoreError::OperationCanceled,
        StoreError::InteractionUnavailable,
        StoreError::Backend,
    ]
}

#[test]
fn new_registration_creates_without_confirmation() {
    let mut f = Fixture::default();
    assert_eq!(f.run(set()).unwrap(), 0);
    assert_eq!(f.calls(), [Operation::Exists, Operation::Create]);
    assert_eq!(f.store.0.borrow().values[NAME], "new-secret");
    assert_eq!(f.prompt.0.borrow().confirmations, 0);
    assert_eq!(f.prompt.0.borrow().secret_inputs, 1);
    assert!(String::from_utf8_lossy(&f.out).contains("Stored"));
    f.assert_no_secret_output();
}

#[test]
fn accepted_replacement_updates_existing_entry() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.answer(true);
    f.run(set()).unwrap();
    assert_eq!(f.calls(), [Operation::Exists, Operation::Replace]);
    assert_eq!(f.store.0.borrow().values[NAME], "new-secret");
    assert_eq!(f.prompt.0.borrow().confirmations, 1);
    assert_eq!(f.prompt.0.borrow().secret_inputs, 1);
    f.assert_no_secret_output();
}

#[test]
fn rejected_replacement_never_prompts_for_secret_or_writes() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.answer(false);
    f.run(set()).unwrap();
    assert_eq!(f.calls(), [Operation::Exists]);
    assert_eq!(f.store.0.borrow().values[NAME], "old-secret");
    assert_eq!(f.prompt.0.borrow().secret_inputs, 0);
    assert_eq!(f.out, b"Aborted.\n");
}

#[test]
fn concurrent_registration_is_preserved_when_replacement_is_rejected() {
    let mut f = Fixture::default();
    f.store.0.borrow_mut().concurrent_value = Some("concurrent-secret".into());
    f.answer(false);
    f.run(set()).unwrap();
    assert_eq!(f.calls(), [Operation::Exists, Operation::Create]);
    assert_eq!(f.store.0.borrow().values[NAME], "concurrent-secret");
    assert_eq!(f.prompt.0.borrow().confirmations, 1);
    assert_eq!(f.prompt.0.borrow().secret_inputs, 1);
    assert_eq!(f.out, b"Aborted.\n");
    f.assert_no_secret_output();
}

#[test]
fn concurrent_registration_is_replaced_only_after_confirmation() {
    let mut f = Fixture::default();
    f.store.0.borrow_mut().concurrent_value = Some("concurrent-secret".into());
    f.answer(true);
    f.run(set()).unwrap();
    assert_eq!(
        f.calls(),
        [Operation::Exists, Operation::Create, Operation::Replace]
    );
    assert_eq!(f.store.0.borrow().values[NAME], "new-secret");
    assert_eq!(f.prompt.0.borrow().confirmations, 1);
    assert_eq!(f.prompt.0.borrow().secret_inputs, 1);
    f.assert_no_secret_output();
}

#[test]
fn disappeared_entry_is_not_recreated_after_confirmation() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.store.0.borrow_mut().disappear_on_replace = true;
    f.answer(true);
    assert!(matches!(
        f.run(set()),
        Err(AppError::Store(StoreError::NotFound))
    ));
    assert_eq!(f.calls(), [Operation::Exists, Operation::Replace]);
    assert!(!f.store.0.borrow().values.contains_key(NAME));
    assert!(f.out.is_empty());
}

#[test]
fn accepted_deletion_removes_only_named_entry() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.store.seed("OTHER_KEY", "other-secret");
    f.answer(true);
    f.run(delete()).unwrap();
    assert_eq!(f.calls(), [Operation::Delete]);
    let state = f.store.0.borrow();
    assert!(!state.values.contains_key(NAME));
    assert!(state.values.contains_key("OTHER_KEY"));
    assert_eq!(state.calls[0].1, NAME);
    assert_eq!(f.prompt.0.borrow().secret_inputs, 0);
    assert!(String::from_utf8_lossy(&f.out).contains("Deleted"));
}

#[test]
fn rejected_deletion_never_calls_store() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.answer(false);
    f.run(delete()).unwrap();
    assert!(f.calls().is_empty());
    assert_eq!(f.store.0.borrow().values[NAME], "old-secret");
    assert_eq!(f.out, b"Aborted.\n");
}

#[test]
fn missing_deletion_reports_not_found() {
    let mut f = Fixture::default();
    f.answer(true);
    f.run(delete()).unwrap();
    assert_eq!(f.calls(), [Operation::Delete]);
    assert!(String::from_utf8_lossy(&f.out).contains("not found"));
    assert!(!String::from_utf8_lossy(&f.out).contains("Deleted"));
}

#[test]
fn registration_lookup_errors_stop_before_confirmation_and_input() {
    for error in failures() {
        let mut f = Fixture::default();
        f.store
            .0
            .borrow_mut()
            .failures
            .insert(Operation::Exists, error);
        assert!(matches!(f.run(set()), Err(AppError::Store(e)) if e == error));
        assert_eq!(f.calls(), [Operation::Exists]);
        assert_eq!(f.prompt.0.borrow().confirmations, 0);
        assert_eq!(f.prompt.0.borrow().secret_inputs, 0);
        assert!(f.out.is_empty());
    }
}

#[test]
fn failed_creation_does_not_report_success() {
    for error in failures() {
        let mut f = Fixture::default();
        f.store
            .0
            .borrow_mut()
            .failures
            .insert(Operation::Create, error);
        assert!(matches!(f.run(set()), Err(AppError::Store(e)) if e == error));
        assert_eq!(f.calls(), [Operation::Exists, Operation::Create]);
        assert!(f.store.0.borrow().values.is_empty());
        assert_eq!(f.prompt.0.borrow().confirmations, 0);
        assert!(f.out.is_empty());
        f.assert_no_secret_output();
    }
}

#[test]
fn failed_replacement_preserves_old_value_and_does_not_report_success() {
    for concurrent in [false, true] {
        for error in failures() {
            let mut f = Fixture::default();
            if concurrent {
                f.store.0.borrow_mut().concurrent_value = Some("concurrent-secret".into());
            } else {
                f.store.seed(NAME, "old-secret");
            }
            f.store
                .0
                .borrow_mut()
                .failures
                .insert(Operation::Replace, error);
            f.answer(true);
            assert!(matches!(f.run(set()), Err(AppError::Store(e)) if e == error));
            assert_eq!(
                f.store.0.borrow().values[NAME],
                if concurrent {
                    "concurrent-secret"
                } else {
                    "old-secret"
                }
            );
            assert!(f.out.is_empty());
            f.assert_no_secret_output();
        }
    }
}

#[test]
fn failed_deletion_preserves_entry_and_does_not_report_success() {
    for error in failures() {
        let mut f = Fixture::default();
        f.store.seed(NAME, "old-secret");
        f.store
            .0
            .borrow_mut()
            .failures
            .insert(Operation::Delete, error);
        f.answer(true);
        assert!(matches!(f.run(delete()), Err(AppError::Store(e)) if e == error));
        assert_eq!(f.calls(), [Operation::Delete]);
        assert_eq!(f.store.0.borrow().values[NAME], "old-secret");
        assert!(f.out.is_empty());
    }
}

#[test]
fn failed_secret_input_does_not_write() {
    let mut f = Fixture::default();
    f.prompt.0.borrow_mut().secret_error = Some(io::ErrorKind::UnexpectedEof);
    assert!(matches!(f.run(set()), Err(AppError::Io(_))));
    assert_eq!(f.calls(), [Operation::Exists]);
    assert!(f.out.is_empty());
}

#[test]
fn failed_confirmation_does_not_modify_existing_or_concurrent_entry() {
    for concurrent in [false, true] {
        let mut f = Fixture::default();
        if concurrent {
            f.store.0.borrow_mut().concurrent_value = Some("concurrent-secret".into());
        } else {
            f.store.seed(NAME, "old-secret");
        }
        f.prompt.0.borrow_mut().confirm_error = Some(io::ErrorKind::UnexpectedEof);
        assert!(matches!(f.run(set()), Err(AppError::Io(_))));
        assert_eq!(
            f.store.0.borrow().values[NAME],
            if concurrent {
                "concurrent-secret"
            } else {
                "old-secret"
            }
        );
        assert!(!f.calls().contains(&Operation::Replace));
        assert!(f.out.is_empty());
    }
}

#[test]
fn failed_delete_confirmation_does_not_access_store() {
    let mut f = Fixture::default();
    f.prompt.0.borrow_mut().confirm_error = Some(io::ErrorKind::UnexpectedEof);
    assert!(matches!(f.run(delete()), Err(AppError::Io(_))));
    assert!(f.calls().is_empty());
    assert!(f.out.is_empty());
}

#[test]
fn run_reads_only_selected_keys_and_propagates_exit_code() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.store.seed("OTHER_KEY", "other-secret");
    f.runner.0.borrow_mut().exit_code = 42;
    assert_eq!(f.run(run(&[NAME])).unwrap(), 42);
    assert_eq!(f.store.0.borrow().calls, [(Operation::Get, NAME.into())]);
    let runner = f.runner.0.borrow();
    assert_eq!(runner.calls.len(), 1);
    assert_eq!(
        runner.calls[0],
        (
            "program".into(),
            vec!["literal argument".into()],
            vec![(NAME.into(), "old-secret".into())]
        )
    );
    f.assert_no_secret_output();
}

#[test]
fn missing_second_key_never_launches_child() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    assert!(matches!(
        f.run(run(&[NAME, "MISSING_KEY"])),
        Err(AppError::Store(StoreError::NotFound))
    ));
    assert!(f.runner.0.borrow().calls.is_empty());
    assert!(f.out.is_empty());
    assert!(f.err.is_empty());
}

#[test]
fn credential_read_errors_never_launch_child() {
    for error in failures() {
        let mut f = Fixture::default();
        f.store
            .0
            .borrow_mut()
            .failures
            .insert(Operation::Get, error);
        assert!(matches!(f.run(run(&[NAME])), Err(AppError::Store(e)) if e == error));
        assert_eq!(f.calls(), [Operation::Get]);
        assert!(f.runner.0.borrow().calls.is_empty());
        assert!(f.out.is_empty());
        assert!(f.err.is_empty());
    }
}

#[test]
fn process_launch_error_does_not_expose_secret() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.runner.0.borrow_mut().failure = Some(io::ErrorKind::NotFound);
    assert!(matches!(f.run(run(&[NAME])), Err(AppError::Process(_))));
    assert!(f.out.is_empty());
    assert!(String::from_utf8_lossy(&f.err).contains("failed to launch"));
    f.assert_no_secret_output();
}

#[test]
fn invalid_names_in_every_command_stop_before_store_and_prompt() {
    for name in ["", "bad-name", "lowercase", "1KEY", "__kagitaba_index__"] {
        for command in [
            Command::Set(SetArgs {
                env_name: name.into(),
            }),
            Command::Delete(DeleteArgs {
                env_name: name.into(),
            }),
            Command::Status(StatusArgs {
                env_name: Some(name.into()),
            }),
            run(&[NAME, name]),
        ] {
            let mut f = Fixture::default();
            assert!(matches!(f.run(command), Err(AppError::InvalidEnvName)));
            assert!(f.calls().is_empty());
            assert_eq!(f.prompt.0.borrow().confirmations, 0);
            assert_eq!(f.prompt.0.borrow().secret_inputs, 0);
            assert!(f.runner.0.borrow().calls.is_empty());
        }
    }
}

#[test]
fn missing_program_stops_before_credential_read() {
    let mut f = Fixture::default();
    let command = Command::Run(RunArgs {
        keys: vec![NAME.into()],
        command: vec![],
    });
    assert!(matches!(f.run(command), Err(AppError::InvalidCommand(_))));
    assert!(f.calls().is_empty());
}

#[test]
fn status_reports_registered_missing_and_empty_without_reading_values() {
    for registered in [false, true] {
        let mut f = Fixture::default();
        if registered {
            f.store.seed(NAME, "old-secret");
        }
        f.run(Command::Status(StatusArgs {
            env_name: Some(NAME.into()),
        }))
        .unwrap();
        assert_eq!(f.calls(), [Operation::Exists]);
        assert!(String::from_utf8_lossy(&f.out).contains(if registered {
            "registered"
        } else {
            "missing"
        }));
        f.assert_no_secret_output();
    }
    let mut f = Fixture::default();
    f.run(Command::Status(StatusArgs { env_name: None }))
        .unwrap();
    assert_eq!(f.out, b"No registered keys.\n");
    assert_eq!(f.calls(), [Operation::List]);
}

#[test]
fn status_list_prints_only_names() {
    let mut f = Fixture::default();
    f.store.seed(NAME, "old-secret");
    f.run(Command::Status(StatusArgs { env_name: None }))
        .unwrap();
    assert_eq!(f.out, format!("{NAME}\n").as_bytes());
    assert_eq!(f.calls(), [Operation::List]);
    f.assert_no_secret_output();
}

#[test]
fn status_errors_do_not_report_registration_or_empty_success() {
    for operation in [Operation::Exists, Operation::List] {
        for error in failures() {
            let mut f = Fixture::default();
            f.store.0.borrow_mut().failures.insert(operation, error);
            let command = Command::Status(StatusArgs {
                env_name: if operation == Operation::Exists {
                    Some(NAME.into())
                } else {
                    None
                },
            });
            assert!(matches!(f.run(command), Err(AppError::Store(e)) if e == error));
            assert!(f.out.is_empty());
        }
    }
}

#[test]
fn stdio_confirmation_accepts_only_explicit_yes_and_rejects_eof() {
    for (answer, accepted) in [
        ("y\n", true),
        ("YES\n", true),
        ("  Yes \r\n", true),
        ("n\n", false),
        ("\n", false),
        ("", false),
        ("yes please\n", false),
    ] {
        let mut input = std::io::Cursor::new(answer.as_bytes());
        let mut output = Vec::new();
        assert_eq!(
            confirm_with_io("Confirm? ", &mut input, &mut output).unwrap(),
            accepted
        );
        assert_eq!(output, b"Confirm? ");
    }
}

struct FailedInput;

impl std::io::Read for FailedInput {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

impl std::io::BufRead for FailedInput {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
    fn consume(&mut self, _: usize) {}
}

struct FailedOutput {
    fail_flush: bool,
}

impl Write for FailedOutput {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.fail_flush {
            Ok(data.len())
        } else {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

#[test]
fn stdio_confirmation_propagates_read_write_and_flush_errors() {
    let result = confirm_with_io("Confirm? ", &mut FailedInput, &mut Vec::new());
    assert!(
        matches!(result, Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::BrokenPipe)
    );
    for fail_flush in [false, true] {
        let mut input = std::io::Cursor::new(b"yes\n");
        let result = confirm_with_io("Confirm? ", &mut input, &mut FailedOutput { fail_flush });
        assert!(
            matches!(result, Err(AppError::Io(error)) if error.kind() == std::io::ErrorKind::BrokenPipe)
        );
        assert_eq!(input.position(), 0);
    }
}

#[path = "history_tests.rs"]
mod history_tests;
