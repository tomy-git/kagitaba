// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::cell::RefCell;

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Open,
    Search(String),
    Read(String),
    Create(String),
    Find(String),
    Set,
    Delete(String),
}

struct FakeApi {
    calls: RefCell<Vec<Call>>,
    open_error: Option<StoreError>,
    error: Option<i32>,
    set_error: Option<i32>,
    bytes: Vec<u8>,
    attributes: Attributes,
}

impl Default for FakeApi {
    fn default() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            open_error: None,
            error: None,
            set_error: None,
            bytes: b"test-only-value".to_vec(),
            attributes: vec![Some(HashMap::from([("acct".into(), "API_KEY".into())]))],
        }
    }
}

impl FakeApi {
    fn call<T>(&self, call: Call, value: T) -> Result<T, i32> {
        self.calls.borrow_mut().push(call);
        self.error.map_or(Ok(value), Err)
    }
}

impl KeychainApi for FakeApi {
    type Keychain = &'static str;
    type Item = &'static str;

    fn open(&self) -> Result<Self::Keychain, StoreError> {
        self.calls.borrow_mut().push(Call::Open);
        self.open_error.map_or(Ok("selected-login-keychain"), Err)
    }

    fn search(
        &self,
        keychain: &Self::Keychain,
        purpose: query::Purpose<'_>,
    ) -> Result<Attributes, i32> {
        assert_eq!(*keychain, "selected-login-keychain");
        let label = match purpose {
            query::Purpose::Exists(account) => format!("exists:{account}"),
            query::Purpose::List => "list".into(),
            query::Purpose::Delete(_) => panic!("search should not delete"),
        };
        self.call(Call::Search(label), self.attributes.clone())
    }

    fn read(&self, keychain: &Self::Keychain, account: &str) -> Result<Vec<u8>, i32> {
        assert_eq!(*keychain, "selected-login-keychain");
        self.call(Call::Read(account.into()), self.bytes.clone())
    }

    fn create(&self, keychain: &Self::Keychain, account: &str, secret: &Secret) -> Result<(), i32> {
        assert_eq!(*keychain, "selected-login-keychain");
        assert!(secret.expose() == "test-only-value");
        self.call(Call::Create(account.into()), ())
    }

    fn find_item(&self, keychain: &Self::Keychain, account: &str) -> Result<Self::Item, i32> {
        assert_eq!(*keychain, "selected-login-keychain");
        self.call(Call::Find(account.into()), "existing-item")
    }

    fn set_password(&self, item: &mut Self::Item, secret: &Secret) -> Result<(), i32> {
        assert_eq!(*item, "existing-item");
        assert!(secret.expose() == "test-only-value");
        self.calls.borrow_mut().push(Call::Set);
        self.set_error.map_or(Ok(()), Err)
    }

    fn delete(&self, keychain: &Self::Keychain, account: &str) -> Result<(), i32> {
        assert_eq!(*keychain, "selected-login-keychain");
        self.call(Call::Delete(account.into()), ())
    }
}

fn secret() -> Secret {
    Secret::new("test-only-value".into())
}

#[test]
fn existence_search_uses_the_selected_keychain_and_exact_account() {
    let store = KeychainStore::new(FakeApi::default());
    assert_eq!(store.exists("API_KEY"), Ok(true));
    assert_eq!(
        *store.0.calls.borrow(),
        [Call::Open, Call::Search("exists:API_KEY".into())]
    );
}

#[test]
fn get_reads_a_secret_from_the_selected_keychain() {
    let store = KeychainStore::new(FakeApi::default());
    assert!(store.get("API_KEY").unwrap().expose() == "test-only-value");
    assert_eq!(
        *store.0.calls.borrow(),
        [Call::Open, Call::Read("API_KEY".into())]
    );
}

#[test]
fn create_passes_the_secret_to_the_selected_keychain() {
    let store = KeychainStore::new(FakeApi::default());
    assert_eq!(store.create("API_KEY", &secret()), Ok(()));
    assert_eq!(
        *store.0.calls.borrow(),
        [Call::Open, Call::Create("API_KEY".into())]
    );
}

#[test]
fn replace_updates_only_the_item_that_was_found() {
    let store = KeychainStore::new(FakeApi::default());
    assert_eq!(store.replace("API_KEY", &secret()), Ok(()));
    assert_eq!(
        *store.0.calls.borrow(),
        [Call::Open, Call::Find("API_KEY".into()), Call::Set]
    );
}

#[test]
fn delete_returns_true_only_after_successful_deletion() {
    let store = KeychainStore::new(FakeApi::default());
    assert_eq!(store.delete("API_KEY"), Ok(true));
    assert_eq!(
        *store.0.calls.borrow(),
        [Call::Open, Call::Delete("API_KEY".into())]
    );
}

#[test]
fn list_names_extracts_the_search_results_from_the_selected_keychain() {
    let store = KeychainStore::new(FakeApi::default());
    assert_eq!(store.list_names(), Ok(vec!["API_KEY".into()]));
    assert_eq!(
        *store.0.calls.borrow(),
        [Call::Open, Call::Search("list".into())]
    );
}

#[test]
fn not_found_has_operation_specific_meaning_and_replace_never_creates() {
    let store = KeychainStore::new(FakeApi {
        error: Some(NOT_FOUND),
        ..FakeApi::default()
    });
    assert_eq!(store.exists("API_KEY"), Ok(false));
    assert!(matches!(store.get("API_KEY"), Err(StoreError::NotFound)));
    assert_eq!(
        store.create("API_KEY", &secret()),
        Err(StoreError::NotFound)
    );
    assert_eq!(store.delete("API_KEY"), Ok(false));
    assert_eq!(store.list_names(), Ok(Vec::new()));
    store.0.calls.borrow_mut().clear();
    assert_eq!(
        store.replace("API_KEY", &secret()),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        *store.0.calls.borrow(),
        [Call::Open, Call::Find("API_KEY".into())]
    );
}

#[test]
fn every_operation_propagates_access_cancellation_interaction_and_backend_failures() {
    for (code, expected) in [
        (DUPLICATE, StoreError::AlreadyExists),
        (AUTH_FAILED, StoreError::AccessDenied),
        (CANCELED, StoreError::OperationCanceled),
        (INTERACTION_NOT_ALLOWED, StoreError::InteractionUnavailable),
        (INTERACTION_REQUIRED, StoreError::InteractionUnavailable),
        (-25244, StoreError::Backend), // errSecInvalidOwnerEdit is not access denial.
        (-1, StoreError::Backend),
    ] {
        let store = KeychainStore::new(FakeApi {
            error: Some(code),
            ..FakeApi::default()
        });
        assert_eq!(store.exists("API_KEY"), Err(expected));
        assert!(matches!(store.get("API_KEY"), Err(error) if error == expected));
        assert_eq!(store.create("API_KEY", &secret()), Err(expected));
        assert_eq!(store.replace("API_KEY", &secret()), Err(expected));
        assert_eq!(store.delete("API_KEY"), Err(expected));
        assert_eq!(store.list_names(), Err(expected));
        assert!(!store.0.calls.borrow().contains(&Call::Set));
    }
}

#[test]
fn replace_propagates_update_errors_after_a_successful_find() {
    for code in [
        NOT_FOUND,
        AUTH_FAILED,
        CANCELED,
        INTERACTION_NOT_ALLOWED,
        INTERACTION_REQUIRED,
        -1,
    ] {
        let store = KeychainStore::new(FakeApi {
            set_error: Some(code),
            ..FakeApi::default()
        });
        assert_eq!(store.replace("API_KEY", &secret()), Err(map_error(code)));
        assert_eq!(
            *store.0.calls.borrow(),
            [Call::Open, Call::Find("API_KEY".into()), Call::Set]
        );
    }
}

#[test]
fn opening_failure_short_circuits_every_operation() {
    for error in [StoreError::AccessDenied, StoreError::Backend] {
        let store = KeychainStore::new(FakeApi {
            open_error: Some(error),
            ..FakeApi::default()
        });
        assert_eq!(store.exists("API_KEY"), Err(error));
        assert!(matches!(store.get("API_KEY"), Err(actual) if actual == error));
        assert_eq!(store.create("API_KEY", &secret()), Err(error));
        assert_eq!(store.replace("API_KEY", &secret()), Err(error));
        assert_eq!(store.delete("API_KEY"), Err(error));
        assert_eq!(store.list_names(), Err(error));
        assert_eq!(
            *store.0.calls.borrow(),
            [
                Call::Open,
                Call::Open,
                Call::Open,
                Call::Open,
                Call::Open,
                Call::Open
            ]
        );
    }
}

#[test]
fn get_rejects_non_utf8_payloads() {
    let store = KeychainStore::new(FakeApi {
        bytes: vec![0xff],
        ..FakeApi::default()
    });
    assert!(matches!(store.get("API_KEY"), Err(StoreError::Backend)));
}

#[test]
fn list_names_rejects_malformed_os_attributes() {
    for attributes in [vec![None], vec![Some(HashMap::new())]] {
        let store = KeychainStore::new(FakeApi {
            attributes,
            ..FakeApi::default()
        });
        assert_eq!(store.list_names(), Err(StoreError::Backend));
    }
}
