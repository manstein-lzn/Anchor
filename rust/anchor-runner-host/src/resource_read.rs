use std::path::{Component, Path};

pub(crate) fn open_resource(base: &Path, relative: &str) -> Result<std::fs::File, std::io::Error> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let invalid =
        || std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid Plugin file path");
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid());
    }
    let target = base.join(path);
    let mut directory = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let components = target
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let last = index + 1 == components.len();
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let fd = openat(
            &directory,
            Path::new(component),
            if last {
                flags
            } else {
                flags | OFlags::DIRECTORY
            },
            Mode::empty(),
        )?;
        if last {
            let file = std::fs::File::from(fd);
            if !file.metadata()?.is_file() {
                return Err(invalid());
            }
            return Ok(file);
        }
        directory = fd;
    }
    Err(invalid())
}
