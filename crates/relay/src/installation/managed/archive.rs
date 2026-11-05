use super::{cancelled, download::MAX_UNPACKED_BYTES};
use crate::error::{Error, Result};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Component, Path},
};
use tokio_util::sync::CancellationToken;

pub(super) fn extract(
    archive: &Path,
    directory: &Path,
    target: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(File::open(archive)?);
    let mut archive = tar::Archive::new(decoder);
    let prefix = format!("package/vendor/{target}");
    let mut files = HashSet::new();
    let mut total = 0u64;
    let mut count = 0usize;
    for entry in archive.entries().map_err(|_| invalid())? {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        count += 1;
        if count > 4096 {
            return Err(invalid());
        }
        let mut entry = entry.map_err(|_| invalid())?;
        let path = entry.path().map_err(|_| invalid())?.into_owned();
        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
            || path
                .to_str()
                .is_none_or(|path| path.len() > 512 || path.contains('\\'))
        {
            return Err(invalid());
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(invalid());
        }
        let size = entry.header().size().map_err(|_| invalid())?;
        total = total.checked_add(size).ok_or_else(invalid)?;
        if total > MAX_UNPACKED_BYTES {
            return Err(invalid());
        }
        let Ok(relative) = path.strip_prefix(&prefix) else {
            continue;
        };
        if relative.as_os_str().is_empty() {
            if kind.is_dir() {
                continue;
            } else {
                return Err(invalid());
            }
        }
        if !files.insert(relative.to_owned()) {
            return Err(invalid());
        }
        let destination = directory.join(relative);
        if kind.is_dir() {
            std::fs::create_dir_all(destination)?;
            continue;
        }
        std::fs::create_dir_all(destination.parent().unwrap())?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let mut buffer = [0u8; 64 * 1024];
        let mut written = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let length = entry.read(&mut buffer).map_err(|_| invalid())?;
            if length == 0 {
                break;
            }
            output.write_all(&buffer[..length])?;
            written += length as u64;
        }
        if written != size {
            return Err(invalid());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if entry.header().mode().map_err(|_| invalid())? & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode))?;
        }
    }
    // Consume the gzip trailer, so malformed/truncated streams cannot look complete.
    let mut decoder = archive.into_inner();
    std::io::copy(
        &mut Read::by_ref(&mut decoder).take(1024 * 1024),
        &mut std::io::sink(),
    )
    .map_err(|_| invalid())?;
    if decoder.read(&mut [0u8; 1]).map_err(|_| invalid())? != 0 {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn executable(directory: &Path, architecture: &str) -> Result<String> {
    let candidates: Vec<_> = ["bin/codex", "codex/codex"]
        .into_iter()
        .filter(|path| directory.join(path).is_file())
        .collect();
    if candidates.len() != 1 {
        return Err(invalid());
    }
    let relative = candidates[0];
    let path = directory.join(relative);
    if path.symlink_metadata()?.is_symlink() {
        return Err(invalid());
    }
    let mut header = [0u8; 64];
    File::open(&path)?
        .read_exact(&mut header)
        .map_err(|_| invalid())?;
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let expected = match architecture {
        "x64" => 62,
        "arm64" => 183,
        _ => return Err(invalid()),
    };
    if header[..7] != *b"\x7fELF\x02\x01\x01" || machine != expected {
        return Err(invalid());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.metadata()?.permissions().mode() & 0o111 == 0 {
            return Err(invalid());
        }
    }
    Ok(relative.into())
}

fn invalid() -> Error {
    Error::new(
        "CODEX_ARCHIVE",
        "Codex 安装包结构、架构或文件内容无效，未更改当前版本。",
    )
}
