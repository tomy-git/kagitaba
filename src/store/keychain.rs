// SPDX-License-Identifier: MPL-2.0

//! ローカル Login Keychain の操作を担当する。共通処理は OS 呼び出し境界のフェイクで検証し、
//! 実際の macOS API への接続だけを NativeApi に分離する。入力や結果表示はここで行わない。

use super::{CredentialStore, Secret, StoreError, extract_names, query};
use std::collections::HashMap;

type Attributes = Vec<Option<HashMap<String, String>>>;

// SecBase.h OSStatus values. Keeping these in the policy lets Linux tests cover
// exactly the same error mapping as the macOS bindings.
const DUPLICATE: i32 = -25299;
const NOT_FOUND: i32 = -25300;
const AUTH_FAILED: i32 = -25293;
const CANCELED: i32 = -128;
const INTERACTION_NOT_ALLOWED: i32 = -25308;
const INTERACTION_REQUIRED: i32 = -25315;

trait KeychainApi {
    type Keychain;
    type Item;
    fn open(&self) -> Result<Self::Keychain, StoreError>;
    fn search(
        &self,
        keychain: &Self::Keychain,
        purpose: query::Purpose<'_>,
    ) -> Result<Attributes, i32>;
    fn read(&self, keychain: &Self::Keychain, account: &str) -> Result<Vec<u8>, i32>;
    fn create(&self, keychain: &Self::Keychain, account: &str, secret: &Secret) -> Result<(), i32>;
    fn find_item(&self, keychain: &Self::Keychain, account: &str) -> Result<Self::Item, i32>;
    fn set_password(&self, item: &mut Self::Item, secret: &Secret) -> Result<(), i32>;
    fn delete(&self, keychain: &Self::Keychain, account: &str) -> Result<(), i32>;
}

pub(super) struct KeychainStore<A>(A);

impl<A> KeychainStore<A> {
    pub(super) fn new(api: A) -> Self {
        Self(api)
    }
}

impl<A: KeychainApi> CredentialStore for KeychainStore<A> {
    fn exists(&self, account: &str) -> Result<bool, StoreError> {
        let keychain = self.0.open()?;
        match self.0.search(&keychain, query::Purpose::Exists(account)) {
            Ok(_) => Ok(true),
            Err(NOT_FOUND) => Ok(false),
            Err(code) => Err(map_error(code)),
        }
    }

    fn get(&self, account: &str) -> Result<Secret, StoreError> {
        let keychain = self.0.open()?;
        let bytes = self.0.read(&keychain, account).map_err(map_error)?;
        match String::from_utf8(bytes) {
            Ok(value) => Ok(Secret::new(value)),
            Err(error) => {
                // UTF-8 として不正な値も秘密として扱い、失敗時のバイト列を消去する。
                drop(zeroize::Zeroizing::new(error.into_bytes()));
                Err(StoreError::Backend)
            }
        }
    }

    fn create(&self, account: &str, secret: &Secret) -> Result<(), StoreError> {
        // 新規登録だけを行い、重複時の更新はしない。上書きの判断はアプリ層へ返す。
        let keychain = self.0.open()?;
        self.0.create(&keychain, account, secret).map_err(map_error)
    }

    fn replace(&self, account: &str, secret: &Secret) -> Result<(), StoreError> {
        let keychain = self.0.open()?;
        // 上書き確認済みでも対象消失は失敗とし、新規作成へ切り替えない。
        let mut item = self.0.find_item(&keychain, account).map_err(map_error)?;
        self.0.set_password(&mut item, secret).map_err(map_error)
    }

    fn delete(&self, account: &str) -> Result<bool, StoreError> {
        let keychain = self.0.open()?;
        match self.0.delete(&keychain, account) {
            Ok(()) => Ok(true),
            Err(NOT_FOUND) => Ok(false),
            Err(code) => Err(map_error(code)),
        }
    }

    fn list_names(&self) -> Result<Vec<String>, StoreError> {
        let keychain = self.0.open()?;
        match self.0.search(&keychain, query::Purpose::List) {
            Ok(attributes) => extract_names(attributes),
            Err(NOT_FOUND) => Ok(Vec::new()),
            Err(code) => Err(map_error(code)),
        }
    }
}

fn map_error(code: i32) -> StoreError {
    // OS の失敗を固定の分類へ変換し、秘密値や未加工のエラー情報を表示層へ渡さない。
    match code {
        DUPLICATE => StoreError::AlreadyExists,
        NOT_FOUND => StoreError::NotFound,
        AUTH_FAILED => StoreError::AccessDenied,
        CANCELED => StoreError::OperationCanceled,
        INTERACTION_NOT_ALLOWED | INTERACTION_REQUIRED => StoreError::InteractionUnavailable,
        _ => StoreError::Backend,
    }
}

#[cfg(target_os = "macos")]
pub(super) struct NativeApi;

#[cfg(target_os = "macos")]
impl KeychainApi for NativeApi {
    type Keychain = security_framework::os::macos::keychain::SecKeychain;
    type Item = security_framework::os::macos::keychain_item::SecKeychainItem;

    fn open(&self) -> Result<Self::Keychain, StoreError> {
        // 保存先はローカル Login Keychain を明示して選ぶ。既定や iCloud のストアに委ねない。
        let home = std::env::var("HOME").map_err(|_| StoreError::Backend)?;
        for name in ["login.keychain-db", "login.keychain"] {
            let path = std::path::Path::new(&home)
                .join("Library/Keychains")
                .join(name);
            if path.exists() {
                return Self::Keychain::open(path).map_err(|error| map_error(error.code()));
            }
        }
        Err(StoreError::Backend)
    }

    fn search(
        &self,
        keychain: &Self::Keychain,
        purpose: query::Purpose<'_>,
    ) -> Result<Attributes, i32> {
        let mut builder = security_framework::item::ItemSearchOptions::new();
        query::configure(&mut builder, keychain, purpose);
        builder
            .search()
            .map(|items| items.into_iter().map(|item| item.simplify_dict()).collect())
            .map_err(|error| error.code())
    }

    fn read(&self, keychain: &Self::Keychain, account: &str) -> Result<Vec<u8>, i32> {
        use security_framework::os::macos::passwords::find_generic_password;
        find_generic_password(
            Some(std::slice::from_ref(keychain)),
            super::SERVICE_NAME,
            account,
        )
        .map(|(password, _)| password.to_vec())
        .map_err(|error| error.code())
    }

    fn create(&self, keychain: &Self::Keychain, account: &str, secret: &Secret) -> Result<(), i32> {
        // 専用サービス名とキー名の範囲で保存し、API の失敗を成功へ置き換えない。
        keychain
            .add_generic_password(super::SERVICE_NAME, account, secret.expose().as_bytes())
            .map_err(|error| error.code())
    }

    fn find_item(&self, keychain: &Self::Keychain, account: &str) -> Result<Self::Item, i32> {
        keychain
            .find_generic_password(super::SERVICE_NAME, account)
            .map(|(_, item)| item)
            .map_err(|error| error.code())
    }

    fn set_password(&self, item: &mut Self::Item, secret: &Secret) -> Result<(), i32> {
        // 検索で得た既存項目だけを更新する。値の有効性確認や保存後照合は別の処理であり未実施。
        item.set_password(secret.expose().as_bytes())
            .map_err(|error| error.code())
    }

    fn delete(&self, keychain: &Self::Keychain, account: &str) -> Result<(), i32> {
        let mut builder = security_framework::item::ItemSearchOptions::new();
        query::configure(&mut builder, keychain, query::Purpose::Delete(account));
        builder.delete().map_err(|error| error.code())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod integration;
#[cfg(test)]
mod tests;
