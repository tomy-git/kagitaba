// SPDX-License-Identifier: MPL-2.0

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fixture {
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = root.join(format!(
            "kagitaba-history-cli-{}-{nonce}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        Self { home }
    }

    fn directory(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home.join("Library/Application Support/kagitaba")
        } else {
            self.home.join(".local/state/kagitaba")
        }
    }

    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kagitaba"))
            .args(args)
            .env("HOME", &self.home)
            .env(
                "KAGITABA_HISTORY_TEST_SECRET",
                "synthetic-env-never-recorded",
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    fn success(&self, args: &[&str], input: &[u8]) -> Output {
        let result = self.run(args, input);
        assert!(result.status.success(), "history command failed");
        result
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn history_cli_is_opt_in_persistent_and_independent_of_keychain() {
    let f = Fixture::new();
    let config = f.success(&["history", "config"], b"");
    assert!(String::from_utf8_lossy(&config.stdout).contains("false"));
    f.success(&["history"], b"");
    assert!(!f.directory().exists());
    f.success(&["history", "enable"], b"");
    f.success(
        &[
            "history",
            "config",
            "--retention-days",
            "15",
            "--max-events",
            "20",
        ],
        b"",
    );
    let config = f.success(&["history", "config"], b"");
    let config = String::from_utf8_lossy(&config.stdout);
    assert!(config.contains("15") && config.contains("20") && config.contains("true"));
    assert_eq!(
        std::fs::metadata(f.directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    // Invalid names fail before any Keychain API; a missing synthetic HOME has no Login Keychain.
    let invalid = f.run(&["set", "synthetic-invalid-name-secret"], b"");
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("synthetic-invalid-name-secret"));
    let missing = f.run(&["set", "VALID_KEY"], b"");
    assert!(!missing.status.success());
    let listing = f.success(
        &["history", "--key", "VALID_KEY", "--failed", "--limit", "1"],
        b"",
    );
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(listing.contains("VALID_KEY"));
    assert!(listing.contains('Z'));
    assert!(!listing.contains("synthetic-invalid-name-secret"));
    assert!(!listing.contains("synthetic-env-never-recorded"));

    for file in std::fs::read_dir(f.directory()).unwrap() {
        let file = file.unwrap().path();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bytes = std::fs::read(file).unwrap();
        for sentinel in [
            b"synthetic-invalid-name-secret".as_slice(),
            b"synthetic-env-never-recorded".as_slice(),
        ] {
            assert!(!bytes.windows(sentinel.len()).any(|part| part == sentinel));
        }
    }

    f.success(&["history", "disable"], b"");
    let before = f.success(&["history"], b"").stdout;
    f.run(&["set", "OTHER_KEY"], b"");
    assert_eq!(f.success(&["history"], b"").stdout, before);
    assert!(
        String::from_utf8_lossy(&f.success(&["history", "clear"], b"n\n").stdout)
            .contains("Aborted")
    );
    assert_eq!(f.success(&["history"], b"").stdout, before);
    f.success(&["history", "clear"], b"yes\n");
    let empty = String::from_utf8(f.success(&["history"], b"").stdout).unwrap();
    assert!(!empty.contains("VALID_KEY"));
    f.success(&["history", "reclaim"], b"");
    assert!(!f.home.join("Library/Keychains").exists());
}

#[test]
fn history_argument_errors_never_echo_supplied_strings_or_create_files() {
    let f = Fixture::new();
    for args in [
        vec![
            "history",
            "config",
            "--max-events",
            "synthetic-private-input",
        ],
        vec!["history", "--limit", "synthetic-private-input"],
        vec!["history", "--key", "synthetic-private-input"],
    ] {
        let result = f.run(&args, b"");
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-private-input"));
        assert!(!String::from_utf8_lossy(&result.stdout).contains("synthetic-private-input"));
    }
    assert!(!f.directory().exists());
}
