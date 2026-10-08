#[cfg(target_os = "macos")]
use std::collections::BTreeSet;

use thiserror::Error;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
const SERVICE_NAME: &str = "dev.kagitaba.kagitaba";
#[cfg(target_os = "macos")]
const INDEX_ACCOUNT: &str = "__kagitaba_index__";

#[derive(Debug)]
pub struct Secret(Zeroizing<String>);

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
    #[error("login keychain is locked")]
    KeychainLocked,
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

    fn load_index(
        &self,
        keychain: &security_framework::os::macos::keychain::SecKeychain,
    ) -> Result<BTreeSet<String>, StoreError> {
        match read_password(keychain, INDEX_ACCOUNT) {
            Ok(secret) => Ok(secret
                .expose()
                .lines()
                .filter(|v| !v.trim().is_empty())
                .map(ToOwned::to_owned)
                .collect()),
            Err(StoreError::NotFound) => Ok(BTreeSet::new()),
            Err(err) => Err(err),
        }
    }

    fn save_index(
        &self,
        keychain: &security_framework::os::macos::keychain::SecKeychain,
        values: &BTreeSet<String>,
    ) -> Result<(), StoreError> {
        let joined = values
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        write_password(keychain, INDEX_ACCOUNT, &Secret::new(joined))
    }
}

#[cfg(target_os = "macos")]
impl CredentialStore for MacosKeychainStore {
    fn exists(&self, env_name: &str) -> Result<bool, StoreError> {
        self.with_keychain(|keychain| match read_password(keychain, env_name) {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound) => Ok(false),
            Err(err) => Err(err),
        })
    }

    fn get(&self, env_name: &str) -> Result<Secret, StoreError> {
        self.with_keychain(|keychain| read_password(keychain, env_name))
    }

    fn set(&self, env_name: &str, secret: &Secret) -> Result<(), StoreError> {
        self.with_keychain(|keychain| {
            write_password(keychain, env_name, secret)?;
            let mut index = self.load_index(keychain)?;
            index.insert(env_name.to_string());
            self.save_index(keychain, &index)
        })
    }

    fn delete(&self, env_name: &str) -> Result<bool, StoreError> {
        self.with_keychain(|keychain| {
            match delete_password(keychain, env_name) {
                Ok(()) => {}
                Err(StoreError::NotFound) => return Ok(false),
                Err(err) => return Err(err),
            }
            let mut index = self.load_index(keychain)?;
            index.remove(env_name);
            self.save_index(keychain, &index)?;
            Ok(true)
        })
    }

    fn list_names(&self) -> Result<Vec<String>, StoreError> {
        self.with_keychain(|keychain| {
            let mut names: Vec<String> = self.load_index(keychain)?.into_iter().collect();
            names.sort();
            Ok(names)
        })
    }
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
fn delete_password(
    keychain: &security_framework::os::macos::keychain::SecKeychain,
    account: &str,
) -> Result<(), StoreError> {
    use security_framework::os::macos::passwords::find_generic_password;

    let keychains = std::slice::from_ref(keychain);
    let (_, item) =
        find_generic_password(Some(keychains), SERVICE_NAME, account).map_err(map_sec_error)?;
    item.delete();
    Ok(())
}

#[cfg(target_os = "macos")]
fn map_sec_error(err: security_framework::base::Error) -> StoreError {
    let code = err.code();
    if code == -25300 {
        return StoreError::NotFound;
    }
    if code == -25293 {
        return StoreError::KeychainLocked;
    }
    if code == -25244 {
        return StoreError::AccessDenied;
    }
    StoreError::Backend
}
