// SPDX-License-Identifier: MPL-2.0

#[cfg(any(target_os = "macos", test))]
use std::collections::{BTreeSet, HashMap};

#[cfg(any(target_os = "macos", test))]
mod keychain;
#[cfg(any(target_os = "macos", test))]
mod query;

use thiserror::Error;
use zeroize::Zeroizing;

#[cfg(any(target_os = "macos", test))]
const SERVICE_NAME: &str = "dev.kagitaba.kagitaba";
#[cfg(any(target_os = "macos", test))]
const INDEX_ACCOUNT: &str = "__kagitaba_index__";

pub struct Secret(Zeroizing<String>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

pub trait CredentialStore {
    fn exists(&self, env_name: &str) -> Result<bool, StoreError>;
    fn get(&self, env_name: &str) -> Result<Secret, StoreError>;
    fn create(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError>;
    fn replace(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError>;
    fn delete(&self, env_name: &str) -> Result<bool, StoreError>;
    fn list_names(&self) -> Result<Vec<String>, StoreError>;
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("credential already exists")]
    AlreadyExists,
    #[error("credential not found")]
    NotFound,
    #[error("access to keychain was denied")]
    AccessDenied,
    #[error("keychain operation was canceled")]
    OperationCanceled,
    #[error("keychain requires user interaction that is unavailable")]
    InteractionUnavailable,
    #[error("kagitaba keychain backend is only available on macOS")]
    UnsupportedPlatform,
    #[error("keychain backend error")]
    Backend,
}

#[cfg(target_os = "macos")]
pub fn default_store() -> Box<dyn CredentialStore> {
    Box::new(keychain::KeychainStore::new(keychain::NativeApi))
}

#[cfg(not(target_os = "macos"))]
pub fn default_store() -> Box<dyn CredentialStore> {
    Box::new(UnsupportedStore)
}

#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
struct UnsupportedStore;

#[cfg(not(target_os = "macos"))]
impl CredentialStore for UnsupportedStore {
    fn exists(&self, _: &str) -> Result<bool, StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }

    fn get(&self, _: &str) -> Result<Secret, StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }

    fn create(&self, _: &str, _: &Secret) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }

    fn replace(&self, _: &str, _: &Secret) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }

    fn delete(&self, _: &str) -> Result<bool, StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }

    fn list_names(&self) -> Result<Vec<String>, StoreError> {
        Err(StoreError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "macos", test))]
fn extract_names(
    items: impl IntoIterator<Item = Option<HashMap<String, String>>>,
) -> Result<Vec<String>, StoreError> {
    let mut names = BTreeSet::new();
    for item in items {
        let mut attributes = item.ok_or(StoreError::Backend)?;
        let account = attributes.remove("acct").ok_or(StoreError::Backend)?;
        // Older versions stored a separate index in this reserved account.
        if account != INDEX_ACCOUNT {
            names.insert(account);
        }
    }
    Ok(names.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attributes(account: &str) -> Option<HashMap<String, String>> {
        Some(HashMap::from([("acct".to_owned(), account.to_owned())]))
    }

    #[test]
    fn list_names_uses_actual_accounts_and_ignores_the_legacy_index() {
        assert_eq!(
            extract_names([
                attributes("Z_KEY"),
                attributes(INDEX_ACCOUNT),
                attributes("A_KEY"),
                attributes("Z_KEY"),
            ]),
            Ok(vec!["A_KEY".to_owned(), "Z_KEY".to_owned()]),
        );
    }

    #[test]
    fn empty_attributes_list_returns_no_names() {
        assert_eq!(extract_names([]), Ok(Vec::new()));
        assert_eq!(extract_names([attributes(INDEX_ACCOUNT)]), Ok(Vec::new()));
    }

    #[test]
    fn unexpected_results_and_missing_accounts_are_backend_errors() {
        assert_eq!(extract_names([None]), Err(StoreError::Backend));
        assert_eq!(
            extract_names([Some(HashMap::new())]),
            Err(StoreError::Backend)
        );
        assert_eq!(
            extract_names([attributes("A_KEY"), Some(HashMap::new())]),
            Err(StoreError::Backend),
        );
    }

    #[test]
    fn secret_debug_redacts_value() {
        let secret = Secret::new("private-value".to_owned());
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        assert_eq!(secret.expose(), "private-value");
    }
}
