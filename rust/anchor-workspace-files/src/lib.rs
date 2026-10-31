#![forbid(unsafe_code)]
use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, mkdirat, open, openat, renameat_with, unlinkat,
};
use std::{
    fs::File,
    io::Write,
    path::{Component, Path},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct WorkspaceFiles(Arc<File>);

impl WorkspaceFiles {
    pub fn new(workspace: impl AsRef<Path>) -> Result<Self, Error> {
        Ok(Self(Arc::new(File::from(open(
            workspace.as_ref(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?))))
    }

    pub fn validate_relative(value: &str) -> Result<&Path, Error> {
        let relative = Path::new(value);
        if relative.components().next().is_none()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err("path must be a relative file path inside the task workspace".into());
        }
        Ok(relative)
    }

    pub fn write_new(&self, relative: &str, bytes: &[u8]) -> Result<(), Error> {
        let relative = Self::validate_relative(relative)?;
        // Pin every directory so concurrent renames and symlink swaps cannot
        // redirect a delivery out of the workspace. This also works without proc.
        let mut parent = self.0.try_clone()?;
        for component in relative.parent().into_iter().flat_map(Path::components) {
            match mkdirat(&parent, component.as_os_str(), Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            parent = File::from(openat(
                &parent,
                component.as_os_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
        }
        let target = relative.file_name().ok_or("path must name a file")?;
        let (temporary_name, mut temporary) = temporary(&parent)?;
        let result = (|| {
            temporary.write_all(bytes)?;
            temporary.sync_all()?;
            renameat_with(
                &parent,
                &temporary_name,
                &parent,
                target,
                RenameFlags::NOREPLACE,
            )?;
            parent.sync_all()?;
            Ok::<_, Error>(())
        })();
        if result.is_err() {
            let _ = unlinkat(&parent, &temporary_name, AtFlags::empty());
        }
        result
    }
}

fn temporary(parent: &File) -> Result<(String, File), Error> {
    for _ in 0..32 {
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            ".anchor-workspace-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
            sequence
        );
        match openat(
            parent,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(file) => return Ok((name, File::from(file))),
            Err(error) if error == rustix::io::Errno::EXIST => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err("unable to allocate a unique temporary workspace file".into())
}
