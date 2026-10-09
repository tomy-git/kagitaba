// SPDX-License-Identifier: MPL-2.0

//! Explicit macOS-only checks. No Login/default Keychain is opened by this test.

use super::*;
use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};
use std::path::PathBuf;

struct IsolatedApi(SecKeychain);

impl KeychainApi for IsolatedApi {
    type Keychain = SecKeychain;
    type Item = <NativeApi as KeychainApi>::Item;

    fn open(&self) -> Result<Self::Keychain, StoreError> {
        Ok(self.0.clone())
    }

    fn search(
        &self,
        keychain: &Self::Keychain,
        purpose: query::Purpose<'_>,
    ) -> Result<Attributes, i32> {
        NativeApi.search(keychain, purpose)
    }

    fn read(&self, keychain: &Self::Keychain, account: &str) -> Result<Vec<u8>, i32> {
        NativeApi.read(keychain, account)
    }

    fn create(&self, keychain: &Self::Keychain, account: &str, secret: &Secret) -> Result<(), i32> {
        NativeApi.create(keychain, account, secret)
    }

    fn find_item(&self, keychain: &Self::Keychain, account: &str) -> Result<Self::Item, i32> {
        NativeApi.find_item(keychain, account)
    }

    fn set_password(&self, item: &mut Self::Item, secret: &Secret) -> Result<(), i32> {
        NativeApi.set_password(item, secret)
    }

    fn delete(&self, keychain: &Self::Keychain, account: &str) -> Result<(), i32> {
        NativeApi.delete(keychain, account)
    }
}

struct TemporaryKeychain {
    directory: PathBuf,
    path: PathBuf,
    created: bool,
}

impl TemporaryKeychain {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "kagitaba-keychain-test-{}-{nonce}",
            std::process::id()
        ));
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let temporary = Self {
            path: directory.join("isolated.keychain"),
            directory,
            created: false,
        };
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&temporary.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        temporary
    }

    fn create(&mut self) -> SecKeychain {
        // Apple StorageManager::shouldAddToSearchList excludes private paths
        // such as this one. Deletion also removes any registration if present.
        let keychain = CreateOptions::new()
            .password("kagitaba-disposable-test-password")
            .prompt_user(false)
            .create(&self.path)
            .unwrap();
        self.created = true;
        keychain
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        // Creation can leave a file before returning an error. This guard owns
        // the unique directory, so even that partial file is safe to target.
        if self.created || self.path.exists() {
            let status = std::process::Command::new("/usr/bin/security")
                .arg("delete-keychain")
                .arg(&self.path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other("temporary Keychain deletion failed"));
            }
            self.created = false;
        }
        if self.directory.exists() {
            std::fs::remove_dir_all(&self.directory)?;
        }
        Ok(())
    }
}

impl Drop for TemporaryKeychain {
    fn drop(&mut self) {
        // Runs on assertion panic as well as success; avoids a second panic.
        if self.cleanup().is_err() {
            eprintln!("temporary Keychain cleanup failed");
        }
    }
}

#[test]
fn os_status_constants_match_security_framework_bindings() {
    use security_framework_sys::base::{errSecAuthFailed, errSecDuplicateItem, errSecItemNotFound};
    assert_eq!(AUTH_FAILED, errSecAuthFailed);
    assert_eq!(DUPLICATE, errSecDuplicateItem);
    assert_eq!(NOT_FOUND, errSecItemNotFound);
}

#[test]
#[ignore = "creates and deletes an isolated macOS Keychain; run explicitly with --ignored"]
fn isolated_keychain_round_trip() {
    let mut temporary = TemporaryKeychain::new();
    // Suppress prompts for this process, preserving the original interaction state.
    let interaction_was_allowed = SecKeychain::user_interaction_allowed().unwrap();
    let _interaction_guard = if interaction_was_allowed {
        Some(SecKeychain::disable_user_interaction().unwrap())
    } else {
        None
    };
    let keychain = temporary.create();
    let store = KeychainStore::new(IsolatedApi(keychain.clone()));
    let first = Secret::new("disposable-first-value".into());
    let second = Secret::new("disposable-second-value".into());

    assert_eq!(store.exists("API_KEY"), Ok(false));
    assert!(matches!(store.get("API_KEY"), Err(StoreError::NotFound)));
    assert_eq!(store.list_names(), Ok(Vec::new()));
    assert_eq!(store.delete("API_KEY"), Ok(false));
    assert_eq!(store.replace("API_KEY", &first), Err(StoreError::NotFound));
    assert_eq!(store.create("API_KEY", &first), Ok(()));
    assert_eq!(
        store.create("API_KEY", &second),
        Err(StoreError::AlreadyExists)
    );
    assert_eq!(store.exists("API_KEY"), Ok(true));
    assert_eq!(store.exists("api_key"), Ok(false));
    assert!(store.get("API_KEY").unwrap().expose() == first.expose());
    assert_eq!(store.replace("API_KEY", &second), Ok(()));
    assert!(store.get("API_KEY").unwrap().expose() == second.expose());

    // Unrelated service and the legacy reserved account must stay out of listing.
    keychain
        .add_generic_password("dev.kagitaba.unrelated-test", "OTHER_KEY", b"disposable")
        .unwrap();
    assert_eq!(store.create(super::super::INDEX_ACCOUNT, &first), Ok(()));
    assert_eq!(store.list_names(), Ok(vec!["API_KEY".into()]));
    assert_eq!(store.delete("api_key"), Ok(false));
    assert_eq!(store.exists("API_KEY"), Ok(true));
    assert_eq!(store.delete("API_KEY"), Ok(true));
    assert_eq!(store.delete("API_KEY"), Ok(false));
    assert_eq!(store.replace("API_KEY", &first), Err(StoreError::NotFound));
    assert_eq!(store.exists("API_KEY"), Ok(false));
    assert_eq!(store.list_names(), Ok(Vec::new()));

    drop(store);
    drop(keychain);
    temporary.cleanup().unwrap();
    assert!(!temporary.directory.exists());
}
