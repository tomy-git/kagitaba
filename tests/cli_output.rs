// SPDX-License-Identifier: MPL-2.0

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kagitaba"))
        .args(args)
        // Parsing tests must not reach the real Login Keychain even if they regress.
        .env("HOME", "/nonexistent/kagitaba-cli-test-home")
        .stdin(Stdio::null())
        .output()
        .expect("launch test binary")
}

#[test]
fn rejected_arguments_are_never_echoed() {
    let marker = "synthetic-sensitive-marker";
    for args in [
        vec!["set", "TEST_KEY", marker],
        vec!["delete", "TEST_KEY", marker],
        vec!["status", "TEST_KEY", marker],
        vec![marker],
        vec!["--synthetic-sensitive-marker"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(stderr.contains("invalid command-line arguments"));
        assert!(!stderr.contains(marker));
    }
}

#[test]
fn missing_arguments_return_usage_error() {
    for args in [vec![], vec!["set"], vec!["run", "--key", "TEST_KEY", "--"]] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8(output.stderr).unwrap().contains("--help"));
    }
}

#[test]
fn invalid_environment_names_are_never_echoed() {
    let marker = "synthetic-sensitive-marker";
    for args in [
        vec!["set", marker],
        vec!["status", marker],
        vec!["delete", marker],
        vec!["run", "--key", marker, "--", "/bin/true"],
    ] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("invalid environment variable name"));
        assert!(!stderr.contains(marker));
    }
}

#[test]
fn help_and_version_remain_available() {
    for args in [vec!["--help"], vec!["set", "--help"], vec!["--version"]] {
        let output = run(&args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("kagitaba")
        );
    }
}
