#[cfg(target_os = "macos")]
use std::collections::BTreeSet;

use thiserror::Error;
use zeroize::Zeroizing;

// SecBase.h constants not exposed by security-framework-sys 2.17.
#[cfg(target_os = "macos")]
const ERR_SEC_USER_CANCELED: i32 = -128;
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
#[cfg(target_os = "macos")]
const ERR_SEC_INTERACTION_REQUIRED: i32 = -25315;

#[cfg(target_os = "macos")]
const SERVICE_NAME: &str = "dev.kagitaba.kagitaba";
#[cfg(target_os = "macos")]
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
    fn set(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError>;
    fn delete(&self, env_name: &str) -> Result<bool, StoreError>;
    fn list_names(&self) -> Result<Vec<String>, StoreError>;
}

#[derive(Debug, Error)]
pub enum StoreError {
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

    fn set(&self, _: &str, _: &Secret) -> Result<(), StoreError> {
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
            let mut query = password_query(keychain, Some(env_name));
            query.load_attributes(true);
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

    fn set(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError> {
        self.with_keychain(|keychain| write_password(keychain, env_name, secret))
    }

    fn delete(&self, env_name: &str) -> Result<bool, StoreError> {
        self.with_keychain(|keychain| {
            deletion_result(password_query(keychain, Some(env_name)).delete())
        })
    }

    fn list_names(&self) -> Result<Vec<String>, StoreError> {
        use security_framework::item::Limit;

        self.with_keychain(|keychain| {
            let mut query = password_query(keychain, None);
            query.load_attributes(true).limit(Limit::All);
            let items = match query.search() {
                Ok(items) => items,
                Err(err) => match map_sec_error(err) {
                    StoreError::NotFound => return Ok(Vec::new()),
                    err => return Err(err),
                },
            };
            let mut names = BTreeSet::new();
            for item in items {
                let mut attributes = item.simplify_dict().ok_or(StoreError::Backend)?;
                let account = attributes.remove("acct").ok_or(StoreError::Backend)?;
                // Older versions stored a separate index in this reserved account.
                if account != INDEX_ACCOUNT {
                    names.insert(account);
                }
            }
            Ok(names.into_iter().collect())
        })
    }
}

#[cfg(target_os = "macos")]
fn password_query(
    keychain: &security_framework::os::macos::keychain::SecKeychain,
    account: Option<&str>,
) -> security_framework::item::ItemSearchOptions {
    use security_framework::item::{ItemClass, ItemSearchOptions};

    let mut query = ItemSearchOptions::new();
    query
        .keychains(std::slice::from_ref(keychain))
        .class(ItemClass::generic_password())
        .service(SERVICE_NAME)
        .case_insensitive(Some(false));
    if let Some(account) = account {
        query.account(account);
    }
    query
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
fn write_password(
    keychain: &security_framework::os::macos::keychain::SecKeychain,
    account: &str,
    secret: &Secret,
) -> Result<(), StoreError> {
    keychain
        .set_generic_password(SERVICE_NAME, account, secret.expose().as_bytes())
        .map_err(map_sec_error)
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
        errSecAuthFailed as ERR_SEC_AUTH_FAILED, errSecItemNotFound as ERR_SEC_ITEM_NOT_FOUND,
    };

    match err.code() {
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
        use security_framework_sys::base::{errSecAuthFailed, errSecItemNotFound};

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
