use object::{
    BinaryFormat, Object, ObjectKind, ObjectSegment, SegmentFlags, elf,
    endian::Endianness,
    read::elf::{FileHeader, ProgramHeader},
};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Runtime {
    pub interpreter: Option<String>,
    pub needed: Vec<String>,
}

pub(super) fn inspect(path: &Path) -> Result<Runtime, String> {
    if fs::metadata(path)
        .map_err(|_| "runtime ELF is unavailable")?
        .len()
        > 1024 * 1024 * 1024
    {
        return Err("runtime ELF exceeds 1 GiB".into());
    }
    let bytes = fs::read(path).map_err(|_| "runtime ELF is unreadable")?;
    let file =
        object::File::parse(bytes.as_slice()).map_err(|_| "runtime binary is not a valid ELF")?;
    if file.format() != BinaryFormat::Elf
        || !file.is_64()
        || !file.is_little_endian()
        || file.architecture() != object::Architecture::X86_64
    {
        return Err("runtime ELF must be 64-bit little-endian x86_64".into());
    }
    if !matches!(file.kind(), ObjectKind::Executable | ObjectKind::Dynamic)
        || file.entry() == 0
        || !file.segments().any(|segment| {
            matches!(segment.flags(), SegmentFlags::Elf { p_type, p_flags }
                if p_type == elf::PT_LOAD && p_flags & elf::PF_X == elf::PF_X)
                && file.entry() >= segment.address()
                && file
                    .entry()
                    .checked_sub(segment.address())
                    .is_some_and(|offset| offset < segment.size())
                && segment.data().is_ok_and(|data| !data.is_empty())
        })
    {
        return Err("runtime binary must have an ELF executable entry point".into());
    }
    let header = elf::FileHeader64::<Endianness>::parse(bytes.as_slice())
        .map_err(|_| "runtime ELF header is invalid")?;
    let endian = header
        .endian()
        .map_err(|_| "runtime ELF endian is invalid")?;
    let headers = header
        .program_headers(endian, bytes.as_slice())
        .map_err(|_| "runtime ELF program header table is invalid")?;
    let mut interpreter = None;
    for program in headers {
        if program.p_type(endian) != elf::PT_INTERP {
            continue;
        }
        if interpreter.is_some() {
            return Err("runtime ELF contains duplicate interpreters".into());
        }
        let data = program
            .data(endian, bytes.as_slice())
            .map_err(|_| "runtime ELF interpreter range is invalid")?;
        let name = data.split(|byte| *byte == 0).next().unwrap_or_default();
        if name.is_empty() || data.last() != Some(&0) {
            return Err("runtime ELF interpreter is invalid".into());
        }
        interpreter = Some(
            std::str::from_utf8(name)
                .map_err(|_| "runtime ELF interpreter is not UTF-8")?
                .to_owned(),
        );
    }
    let mut needed = Vec::new();
    for library in file
        .import_libraries()
        .map_err(|_| "runtime ELF dynamic library table is invalid")?
    {
        let library = library.map_err(|_| "runtime ELF dynamic library entry is invalid")?;
        let name = std::str::from_utf8(library.name())
            .map_err(|_| "runtime ELF library name is not UTF-8")?;
        if name.is_empty() {
            return Err("runtime ELF library name is empty".into());
        }
        needed.push(name.to_owned());
    }
    needed.sort();
    needed.dedup();
    Ok(Runtime {
        interpreter,
        needed,
    })
}
