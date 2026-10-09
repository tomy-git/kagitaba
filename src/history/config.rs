// SPDX-License-Identifier: MPL-2.0

use super::{HistoryError, OperationId, Settings, SettingsUpdate, paths::PrivateDirectory};
use rustix::fs::{self, AtFlags, FlockOperation};
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

pub(super) const FILE: &str = "config";

pub(super) fn read(directory: &PrivateDirectory) -> Result<Settings, HistoryError> {
    let Some(fd) = directory.open_file(FILE, false, false)? else {
        return Ok(Settings::default());
    };
    let mut bytes = Vec::new();
    File::from(fd)
        .take(1025)
        .read_to_end(&mut bytes)
        .map_err(|_| HistoryError::Unavailable)?;
    if bytes.len() > 1024 {
        return Err(HistoryError::Configuration);
    }
    let value = std::str::from_utf8(&bytes).map_err(|_| HistoryError::Configuration)?;
    let mut enabled = None;
    let mut retention_days = None;
    let mut max_events = None;
    for line in value.lines() {
        let (key, value) = line.split_once('=').ok_or(HistoryError::Configuration)?;
        match key {
            "enabled" if enabled.is_none() => {
                enabled = Some(match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err(HistoryError::Configuration),
                })
            }
            "retention_days" if retention_days.is_none() => {
                retention_days = Some(value.parse().map_err(|_| HistoryError::Configuration)?)
            }
            "max_events" if max_events.is_none() => {
                max_events = Some(value.parse().map_err(|_| HistoryError::Configuration)?)
            }
            _ => return Err(HistoryError::Configuration),
        }
    }
    validate(Settings {
        enabled: enabled.ok_or(HistoryError::Configuration)?,
        retention_days: retention_days.ok_or(HistoryError::Configuration)?,
        max_events: max_events.ok_or(HistoryError::Configuration)?,
    })
}

pub(super) fn validate(settings: Settings) -> Result<Settings, HistoryError> {
    if !(1..=3650).contains(&settings.retention_days)
        || !(2..=1_000_000).contains(&settings.max_events)
    {
        return Err(HistoryError::Configuration);
    }
    Ok(settings)
}

pub(super) fn updated(
    mut settings: Settings,
    update: SettingsUpdate,
) -> Result<Settings, HistoryError> {
    if let Some(enabled) = update.enabled {
        settings.enabled = enabled;
    }
    if let Some(days) = update.retention_days {
        settings.retention_days = days;
    }
    if let Some(events) = update.max_events {
        settings.max_events = events;
    }
    validate(settings)
}

pub(super) fn lock(directory: &PrivateDirectory) -> Result<OwnedFd, HistoryError> {
    let fd = directory
        .open_file("history.lock", true, false)?
        .ok_or(HistoryError::Unavailable)?;
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match fs::flock(&fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(fd),
            Err(rustix::io::Errno::WOULDBLOCK) => {
                if Instant::now() >= deadline {
                    return Err(HistoryError::Busy);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err(HistoryError::Unavailable),
        }
    }
}

pub(super) fn write(directory: &PrivateDirectory, settings: Settings) -> Result<(), HistoryError> {
    let name = format!("config.{}.tmp", OperationId::new()?.as_str());
    let fd = directory
        .open_file(&name, true, true)?
        .ok_or(HistoryError::Unavailable)?;
    let mut file = File::from(fd);
    let result = (|| {
        write!(
            file,
            "enabled={}\nretention_days={}\nmax_events={}\n",
            settings.enabled, settings.retention_days, settings.max_events
        )
        .map_err(|_| HistoryError::Unavailable)?;
        file.sync_all().map_err(|_| HistoryError::Unavailable)?;
        fs::renameat(&directory.fd, &name, &directory.fd, FILE)
            .map_err(|_| HistoryError::Unavailable)?;
        directory.sync()
    })();
    if result.is_err() {
        let _ = fs::unlinkat(&directory.fd, &name, AtFlags::empty());
    }
    result
}
