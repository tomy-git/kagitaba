// SPDX-License-Identifier: MPL-2.0

//! Descriptor-based checks prevent traversing links or repairing unsafe files.

use super::HistoryError;
use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::{
    fs::File,
    os::fd::OwnedFd,
    path::{Component, Path, PathBuf},
};

pub(super) struct PrivateDirectory {
    pub path: PathBuf,
    pub fd: OwnedFd,
}

fn path_error(error: rustix::io::Errno) -> HistoryError {
    if matches!(
        error,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR | rustix::io::Errno::ISDIR
    ) {
        HistoryError::UnsafePath
    } else {
        HistoryError::Unavailable
    }
}

fn check_directory(fd: &OwnedFd, private: bool) -> Result<(), HistoryError> {
    let stat = fs::fstat(fd).map_err(path_error)?;
    check_directory_stat(&stat, private)
}

fn check_directory_stat(stat: &fs::Stat, private: bool) -> Result<(), HistoryError> {
    let uid = rustix::process::geteuid().as_raw();
    let mode = stat.st_mode;
    if fs::FileType::from_raw_mode(stat.st_mode) != fs::FileType::Directory {
        return Err(HistoryError::UnsafePath);
    }
    if private {
        if stat.st_uid != uid || mode & 0o7777 != 0o700 {
            return Err(HistoryError::UnsafePath);
        }
    } else if (stat.st_uid != uid && stat.st_uid != 0)
        || (mode & 0o022 != 0 && !(stat.st_uid == 0 && mode & 0o1000 != 0))
    {
        return Err(HistoryError::UnsafePath);
    }
    Ok(())
}

fn check_file(fd: &OwnedFd) -> Result<(), HistoryError> {
    let stat = fs::fstat(fd).map_err(path_error)?;
    check_file_stat(&stat)
}

fn check_file_stat(stat: &fs::Stat) -> Result<(), HistoryError> {
    if fs::FileType::from_raw_mode(stat.st_mode) != fs::FileType::RegularFile
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
    {
        return Err(HistoryError::UnsafePath);
    }
    Ok(())
}

impl PrivateDirectory {
    pub fn open(path: &Path, create: bool) -> Result<Option<Self>, HistoryError> {
        if !path.is_absolute() {
            return Err(HistoryError::UnsafePath);
        }
        let components: Vec<_> = path.components().collect();
        if components.len() < 2
            || components
                .iter()
                .skip(1)
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(HistoryError::UnsafePath);
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut fd = fs::open("/", flags, Mode::empty()).map_err(path_error)?;
        check_directory(&fd, false)?;
        for (index, component) in components.iter().enumerate().skip(1) {
            let Component::Normal(name) = component else {
                unreachable!()
            };
            let next = match fs::openat(&fd, *name, flags, Mode::empty()) {
                Ok(next) => next,
                Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
                Err(rustix::io::Errno::NOENT) => {
                    match fs::mkdirat(&fd, *name, Mode::from_raw_mode(0o700)) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => return Err(path_error(error)),
                    }
                    fs::openat(&fd, *name, flags, Mode::empty()).map_err(path_error)?
                }
                Err(error) => return Err(path_error(error)),
            };
            check_directory(&next, index + 1 == components.len())?;
            fd = next;
        }
        let directory = Self {
            path: path.into(),
            fd,
        };
        directory.validate_children()?;
        Ok(Some(directory))
    }

    pub fn validate_children(&self) -> Result<(), HistoryError> {
        for entry in std::fs::read_dir(&self.path).map_err(|_| HistoryError::Unavailable)? {
            let entry = entry.map_err(|_| HistoryError::Unavailable)?;
            // Unknown future backups/temp files also have to meet the private-file contract.
            self.open_file(entry.file_name().as_os_str(), false, false)?;
        }
        Ok(())
    }

    pub fn open_file(
        &self,
        name: impl AsRef<std::ffi::OsStr>,
        create: bool,
        exclusive: bool,
    ) -> Result<Option<OwnedFd>, HistoryError> {
        match fs::statat(&self.fd, name.as_ref(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => check_file_stat(&stat)?,
            Err(rustix::io::Errno::NOENT) => {}
            Err(error) => return Err(path_error(error)),
        }
        let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if create {
            flags |= OFlags::CREATE;
        }
        if exclusive {
            flags |= OFlags::EXCL;
        }
        let fd = match fs::openat(&self.fd, name.as_ref(), flags, Mode::from_raw_mode(0o600)) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
            Err(error) => return Err(path_error(error)),
        };
        check_file(&fd)?;
        Ok(Some(fd))
    }

    pub fn identity(&self, name: &str, original: &OwnedFd) -> Result<(), HistoryError> {
        let before = fs::fstat(original).map_err(path_error)?;
        let after = fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(path_error)?;
        if before.st_dev != after.st_dev || before.st_ino != after.st_ino {
            return Err(HistoryError::UnsafePath);
        }
        self.open_file(name, false, false)?
            .ok_or(HistoryError::UnsafePath)?;
        Ok(())
    }

    pub fn sync(&self) -> Result<(), HistoryError> {
        File::from(self.fd.try_clone().map_err(|_| HistoryError::Unavailable)?)
            .sync_all()
            .map_err(|_| HistoryError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_metadata_must_match_current_user_or_safe_root_ancestor() {
        let fd = fs::open("/", OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).unwrap();
        let mut stat = fs::fstat(&fd).unwrap();
        stat.st_mode = 0o040700;
        stat.st_uid = rustix::process::geteuid().as_raw();
        assert_eq!(check_directory_stat(&stat, true), Ok(()));
        stat.st_uid = stat.st_uid.wrapping_add(1).max(1);
        assert_eq!(
            check_directory_stat(&stat, true),
            Err(HistoryError::UnsafePath)
        );
        assert_eq!(
            check_directory_stat(&stat, false),
            Err(HistoryError::UnsafePath)
        );
        stat.st_uid = 0;
        stat.st_mode = 0o041777;
        assert_eq!(check_directory_stat(&stat, false), Ok(()));
        stat.st_mode = 0o040777;
        assert_eq!(
            check_directory_stat(&stat, false),
            Err(HistoryError::UnsafePath)
        );
        stat.st_mode = 0o100600;
        stat.st_nlink = 1;
        stat.st_uid = rustix::process::geteuid().as_raw();
        assert_eq!(check_file_stat(&stat), Ok(()));
        stat.st_uid = stat.st_uid.wrapping_add(1);
        assert_eq!(check_file_stat(&stat), Err(HistoryError::UnsafePath));
    }
}
