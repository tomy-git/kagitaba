// SPDX-License-Identifier: MPL-2.0

mod model;
pub use model::*;

mod config;
mod db;
mod paths;

use std::path::PathBuf;

/// A private history directory, independent of Keychain storage.
pub struct HistoryStore {
    directory: Option<PathBuf>,
}

impl HistoryStore {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
        }
    }

    pub fn from_home() -> Self {
        let directory = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| {
                let home = PathBuf::from(home);
                if cfg!(target_os = "macos") {
                    home.join("Library/Application Support/kagitaba")
                } else {
                    home.join(".local/state/kagitaba")
                }
            });
        Self { directory }
    }

    fn directory(&self, create: bool) -> Result<Option<paths::PrivateDirectory>, HistoryError> {
        paths::PrivateDirectory::open(
            self.directory.as_deref().ok_or(HistoryError::Unavailable)?,
            create,
        )
    }
}

impl History for HistoryStore {
    fn settings(&self) -> Result<Settings, HistoryError> {
        match self.directory(false)? {
            Some(directory) => config::read(&directory),
            None => Ok(Settings::default()),
        }
    }

    fn configure(&self, update: SettingsUpdate) -> Result<Settings, HistoryError> {
        config::updated(Settings::default(), update)?;
        let directory = self.directory(true)?.ok_or(HistoryError::Unavailable)?;
        let _lock = config::lock(&directory)?;
        let settings = config::updated(config::read(&directory)?, update)?;
        config::write(&directory, settings)?;
        Ok(settings)
    }

    fn record(&self, record: &Record) -> Result<(), HistoryError> {
        // A missing or disabled configuration never creates a directory or DB.
        let Some(directory) = self.directory(false)? else {
            return Ok(());
        };
        if !config::read(&directory)?.enabled {
            return Ok(());
        }
        let _lock = config::lock(&directory)?;
        let settings = config::read(&directory)?;
        if !settings.enabled {
            return Ok(());
        }
        let mut database =
            db::Database::open(&directory, true)?.ok_or(HistoryError::Unavailable)?;
        let now = utc_ms()?;
        db::record(&mut database.connection, record, settings, now)?;
        database.check()
    }

    fn list(&self, query: &Query) -> Result<Vec<Entry>, HistoryError> {
        if !(1..=1000).contains(&query.limit) {
            return Err(HistoryError::Configuration);
        }
        let Some(directory) = self.directory(false)? else {
            return Ok(Vec::new());
        };
        // An empty directory stays empty until configuration enables recording.
        if directory.open_file(db::FILE, false, false)?.is_none() {
            return Ok(Vec::new());
        }
        let _lock = config::lock(&directory)?;
        let settings = config::read(&directory)?;
        let Some(mut database) = db::Database::open(&directory, false)? else {
            return Ok(Vec::new());
        };
        let entries = db::list(&mut database.connection, query, settings, utc_ms()?)?;
        database.check()?;
        Ok(entries)
    }

    fn clear(&self) -> Result<(), HistoryError> {
        let Some(directory) = self.directory(false)? else {
            return Ok(());
        };
        let _lock = config::lock(&directory)?;
        let Some(mut database) = db::Database::open(&directory, false)? else {
            return Ok(());
        };
        let transaction = database
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db::error)?;
        transaction
            .execute("DELETE FROM events", [])
            .map_err(db::error)?;
        transaction.commit().map_err(db::error)?;
        database.reclaim().map_err(|_| HistoryError::Reclaim)
    }

    fn reclaim(&self) -> Result<(), HistoryError> {
        let Some(directory) = self.directory(false)? else {
            return Ok(());
        };
        let _lock = config::lock(&directory)?;
        let Some(database) = db::Database::open(&directory, false)? else {
            return Ok(());
        };
        database.reclaim()
    }
}

#[cfg(test)]
mod tests;

fn utc_ms() -> Result<i64, HistoryError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| HistoryError::Unavailable)?
        .as_millis()
        .try_into()
        .map_err(|_| HistoryError::Unavailable)
}
