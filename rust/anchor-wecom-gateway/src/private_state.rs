use std::{
    fs::{self, File},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt},
    },
    path::{Component, Path, PathBuf},
};

use rustix::fs::{
    AtFlags, FlockOperation, Mode, OFlags, flock, fsync, mkdirat, open, openat, renameat, unlinkat,
};
use serde_json::json;

use crate::{GatewayConfig, GatewayError};

pub(crate) struct PrivateState {
    directory: File,
    _lease: File,
    path: PathBuf,
    published: bool,
}

impl PrivateState {
    pub fn acquire(config: &GatewayConfig) -> Result<Self, GatewayError> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut directory =
            open("/", flags, Mode::empty()).map_err(|_| GatewayError::PrivateState)?;
        for component in config.state_dir.components() {
            if let Component::Normal(name) = component {
                match mkdirat(&directory, name, Mode::RWXU) {
                    Ok(()) => fsync(&directory).map_err(|_| GatewayError::PrivateState)?,
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(_) => return Err(GatewayError::PrivateState),
                }
                directory = openat(&directory, name, flags, Mode::empty())
                    .map_err(|_| GatewayError::PrivateState)?;
            }
        }
        let directory = File::from(directory);
        let metadata = directory
            .metadata()
            .map_err(|_| GatewayError::PrivateState)?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o700
        {
            return Err(GatewayError::PrivateState);
        }
        let lease = regular(&directory, "gateway.lock")?;
        flock(&lease, FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| GatewayError::AlreadyRunning)?;
        let state = Self {
            directory,
            _lease: lease,
            path: config.state_dir.clone(),
            published: false,
        };
        state.remove_stale("control.sock", true)?;
        state.remove_stale("control.json", false)?;
        regular(&state.directory, "delivery.sqlite")?;
        for name in ["delivery.sqlite-journal", "events.sqlite"] {
            if state.path.join(name).symlink_metadata().is_ok() {
                regular(&state.directory, name)?;
                if name == "events.sqlite" {
                    return Err(GatewayError::Invalid(
                        "legacy Python ledger cannot become native gateway state",
                    ));
                }
            }
        }
        Ok(state)
    }

    pub fn database_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/self/fd/{}/delivery.sqlite",
            self.directory.as_raw_fd()
        ))
    }

    pub fn socket_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/self/fd/{}/control.sock",
            self.directory.as_raw_fd()
        ))
    }

    pub fn publish(&mut self, config: &GatewayConfig) -> Result<(), GatewayError> {
        let socket = config.socket_path();
        let metadata = self
            .socket_path()
            .symlink_metadata()
            .map_err(|_| GatewayError::PrivateState)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(GatewayError::PrivateState);
        }
        fs::set_permissions(
            self.socket_path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|_| GatewayError::PrivateState)?;
        let temporary = format!(".control-{}", uuid::Uuid::new_v4());
        let mut file = File::from(
            openat(
                &self.directory,
                &temporary,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| GatewayError::PrivateState)?,
        );
        let data = serde_json::to_vec(&json!({"socket": socket, "token": config.control_token}))
            .map_err(|_| GatewayError::PrivateState)?;
        let result = (|| {
            file.write_all(&data)
                .map_err(|_| GatewayError::PrivateState)?;
            file.sync_all().map_err(|_| GatewayError::PrivateState)?;
            renameat(&self.directory, &temporary, &self.directory, "control.json")
                .map_err(|_| GatewayError::PrivateState)?;
            fsync(&self.directory).map_err(|_| GatewayError::PrivateState)?;
            Ok(())
        })();
        let _ = unlinkat(&self.directory, &temporary, AtFlags::empty());
        self.published = result.is_ok();
        result
    }

    pub fn cleanup(&mut self) {
        for name in ["control.json", "control.sock"] {
            let _ = unlinkat(&self.directory, name, AtFlags::empty());
        }
        let _ = fsync(&self.directory);
        self.published = false;
    }

    fn remove_stale(&self, name: &str, socket: bool) -> Result<(), GatewayError> {
        match self.path.join(name).symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(GatewayError::PrivateState),
            Ok(metadata) => {
                if metadata.uid() != rustix::process::geteuid().as_raw()
                    || metadata.mode() & 0o077 != 0
                    || (socket && !metadata.file_type().is_socket())
                    || (!socket && (!metadata.is_file() || metadata.nlink() != 1))
                {
                    return Err(GatewayError::PrivateState);
                }
                unlinkat(&self.directory, name, AtFlags::empty())
                    .map_err(|_| GatewayError::PrivateState)
            }
        }
    }
}

impl Drop for PrivateState {
    fn drop(&mut self) {
        if self.published {
            self.cleanup();
        }
    }
}

fn regular(directory: &File, name: impl AsRef<Path>) -> Result<File, GatewayError> {
    let file = File::from(
        openat(
            directory,
            name.as_ref(),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| GatewayError::PrivateState)?,
    );
    let metadata = file.metadata().map_err(|_| GatewayError::PrivateState)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(GatewayError::PrivateState);
    }
    Ok(file)
}
