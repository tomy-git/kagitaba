// SPDX-License-Identifier: MPL-2.0

#[cfg(any(target_os = "macos", test))]
use std::collections::{BTreeSet, HashMap};

#[cfg(any(target_os = "macos", test))]
mod query;

use thiserror::Error;
use zeroize::Zeroizing;

// SecBase.h constants not exposed by security-framework-sys 2.17.
#[cfg(target_os = "macos")]
const ERR_SEC_USER_CANCELED: i32 = -128;
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_REQUIRED: i32 = -25315;

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
    Box::new(MacosKeychainStore::new())
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

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacosKeychainStore;

#[cfg(target_os = "macos")]
impl MacosKeychainStore {
    fn new() -> Self {
        Self
    }

    fn with_keychain<F, T>(&self, op: F) -> Result<T, StoreError>
    where
        F: FnOnce(&security_framework::os::macos::keychain::SecKeychain) -> Result<T, StoreError>,
    {
        let keychain = login_keychain()?;
        op(&keychain)
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for MacosKeychainStore {
    fn exists(&self, env_name: &str) -> Result<bool, StoreError> {
        self.with_keychain(|keychain| {
            let query = password_query(keychain, query::Purpose::Exists(env_name));
            match query.search() {
                Ok(_) => Ok(true),
                Err(err) => match map_sec_error(err) {
                    StoreError::NotFound => Ok(false),
                    err => Err(err),
                },
            }
        })
    }

    fn get(&self, env_name: &str) -> Result<Secret, StoreError> {
        self.with_keychain(|keychain| read_password(keychain, env_name))
    }

    fn create(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError> {
        self.with_keychain(|keychain| {
            keychain
                .add_generic_password(SERVICE_NAME, env_name, secret.expose().as_bytes())
                .map_err(map_sec_error)
        })
    }

    fn replace(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError> {
        self.with_keychain(|keychain| {
            let (_, mut item) = keychain
                .find_generic_password(SERVICE_NAME, env_name)
                .map_err(map_sec_error)?;
            item.set_password(secret.expose().as_bytes())
                .map_err(map_sec_error)
        })
    }

    fn delete(&self, env_name: &str) -> Result<bool, StoreError> {
        self.with_keychain(|keychain| {
            deletion_result(password_query(keychain, query::Purpose::Delete(env_name)).delete())
        })
    }

    fn list_names(&self) -> Result<Vec<String>, StoreError> {
        self.with_keychain(|keychain| {
            let query = password_query(keychain, query::Purpose::List);
            let items = match query.search() {
                Ok(items) => items,
                Err(err) => match map_sec_error(err) {
                    StoreError::NotFound => return Ok(Vec::new()),
                    err => return Err(err),
                },
            };
            extract_names(items.into_iter().map(|item| item.simplify_dict()))
        })
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

#[cfg(target_os = "macos")]
fn password_query(
    keychain: &security_framework::os::macos::keychain::SecKeychain,
    purpose: query::Purpose<'_>,
) -> security_framework::item::ItemSearchOptions {
    let mut builder = security_framework::item::ItemSearchOptions::new();
    query::configure(&mut builder, keychain, purpose);
    builder
}

#[cfg(target_os = "macos")]
fn login_keychain() -> Result<security_framework::os::macos::keychain::SecKeychain, StoreError> {
    use std::path::PathBuf;

    let home = std::env::var("HOME").map_err(|_| StoreError::Backend)?;
    for name in ["login.keychain-db", "login.keychain"] {
        let mut path = PathBuf::from(&home);
        path.push("Library");
        path.push("Keychains");
        path.push(name);
        if path.exists() {
            return security_framework::os::macos::keychain::SecKeychain::open(path)
                .map_err(map_sec_error);
        }
    }
    Err(StoreError::Backend)
}

#[cfg(target_os = "macos")]
fn read_password(
    keychain: &security_framework::os::macos::keychain::SecKeychain,
    account: &str,
) -> Result<Secret, StoreError> {
    use security_framework::os::macos::passwords::find_generic_password;

    let keychains = std::slice::from_ref(keychain);
    let (password, _) =
        find_generic_password(Some(keychains), SERVICE_NAME, account).map_err(map_sec_error)?;
    String::from_utf8(password.to_vec())
        .map(Secret::new)
        .map_err(|_| StoreError::Backend)
}

#[cfg(target_os = "macos")]
fn deletion_result(result: security_framework::base::Result<()>) -> Result<bool, StoreError> {
    match result {
        Ok(()) => Ok(true),
        Err(err) => match map_sec_error(err) {
            StoreError::NotFound => Ok(false),
            err => Err(err),
        },
    }
}

#[cfg(target_os = "macos")]
fn map_sec_error(err: security_framework::base::Error) -> StoreError {
    use security_framework_sys::base::{
        errSecAuthFailed as ERR_SEC_AUTH_FAILED, errSecDuplicateItem as ERR_SEC_DUPLICATE_ITEM,
        errSecItemNotFound as ERR_SEC_ITEM_NOT_FOUND,
    };

    match err.code() {
        ERR_SEC_DUPLICATE_ITEM => StoreError::AlreadyExists,
        ERR_SEC_ITEM_NOT_FOUND => StoreError::NotFound,
        ERR_SEC_AUTH_FAILED => StoreError::AccessDenied,
        ERR_SEC_USER_CANCELED => StoreError::OperationCanceled,
        ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_INTERACTION_REQUIRED => {
            StoreError::InteractionUnavailable
        }
        _ => StoreError::Backend,
    }
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

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_deletion_is_never_reported_as_success() {
        use security_framework::base::Error;
        use security_framework_sys::base::{errSecAuthFailed, errSecItemNotFound};

        assert!(matches!(deletion_result(Ok(())), Ok(true)));
        assert!(matches!(
            deletion_result(Err(Error::from_code(errSecItemNotFound))),
            Ok(false)
        ));
        assert!(matches!(
            deletion_result(Err(Error::from_code(errSecAuthFailed))),
            Err(StoreError::AccessDenied)
        ));
        assert!(matches!(
            deletion_result(Err(Error::from_code(-1))),
            Err(StoreError::Backend)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_errors_do_not_assume_the_keychain_is_locked() {
        use security_framework::base::Error;
        use security_framework_sys::base::{
            errSecAuthFailed, errSecDuplicateItem, errSecItemNotFound,
        };

        assert_eq!(
            map_sec_error(Error::from_code(errSecDuplicateItem)),
            StoreError::AlreadyExists
        );
        assert!(matches!(
            map_sec_error(Error::from_code(errSecItemNotFound)),
            StoreError::NotFound
        ));
        assert!(matches!(
            map_sec_error(Error::from_code(errSecAuthFailed)),
            StoreError::AccessDenied
        ));
        assert!(matches!(
            map_sec_error(Error::from_code(ERR_SEC_USER_CANCELED)),
            StoreError::OperationCanceled
        ));
        for code in [
            ERR_SEC_INTERACTION_NOT_ALLOWED,
            ERR_SEC_INTERACTION_REQUIRED,
        ] {
            assert!(matches!(
                map_sec_error(Error::from_code(code)),
                StoreError::InteractionUnavailable
            ));
        }
        // -25244 is errSecInvalidOwnerEdit, not an authentication failure.
        assert!(matches!(
            map_sec_error(Error::from_code(-25244)),
            StoreError::Backend
        ));
    }
}
