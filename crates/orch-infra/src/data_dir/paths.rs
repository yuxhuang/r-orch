//! R33/R107: only a daemon-computed SHA-256 selects a project record.
use super::confined::{ConfinedDir, ConfinedError, FileAccess};
use super::{DataDirError, private_dir};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn invalid(message: impl Into<String>) -> DataDirError {
    DataDirError::InvalidLocation(message.into())
}

pub fn validate_project_id(id: &str) -> Result<(), DataDirError> {
    if id.is_empty()
        || id.len() > 63
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid(
            "project id must be 1 to 63 ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

pub fn default_name(project: &Path) -> Result<String, DataDirError> {
    use std::os::unix::ffi::OsStrExt;
    let canonical = project.canonicalize()?;
    let base = canonical.file_name().unwrap_or_default().to_string_lossy();
    let slug: String = base
        .chars()
        .map(|c| {
            let lower = c.to_ascii_lowercase();
            if lower.is_ascii_alphanumeric() || "._-".contains(lower) {
                lower
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    let hash = format!("{:x}", Sha256::digest(canonical.as_os_str().as_bytes()));
    Ok(format!("{slug}-{}", &hash[..12]))
}

fn record_name(id: &str) -> Result<String, DataDirError> {
    validate_project_id(id)?;
    Ok(format!("{:x}", Sha256::digest(id.as_bytes())))
}

fn parse_record(bytes: &[u8]) -> Result<PathBuf, DataDirError> {
    if bytes.len() > 4096 {
        return Err(invalid("project record exceeds 4096 bytes"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("project record is not UTF-8"))?;
    let line = text
        .strip_suffix('\n')
        .filter(|s| !s.contains(['\n', '\r', '\0']))
        .ok_or_else(|| invalid("project record must hold one absolute path and a newline"))?;
    let path = PathBuf::from(line);
    if !path.is_absolute() {
        return Err(invalid("project record must name an absolute path"));
    }
    private_dir(&ConfinedDir::open(&path)?)?;
    Ok(path)
}

pub fn read_project_record(data_home: &Path, id: &str) -> Result<PathBuf, DataDirError> {
    read_project_record_with(data_home, id, false)
}

/// Import refusals preserve the auxiliary record's metadata, including atime.
/// Use the ordinary confined reader and parser, but refuse if reads cannot be
/// made without updating access time. No ordinary-read fallback is permitted.
pub fn read_project_record_for_import(data_home: &Path, id: &str) -> Result<PathBuf, DataDirError> {
    read_project_record_with(data_home, id, true)
}

fn preserve_access_time(file: &std::fs::File) -> Result<(), DataDirError> {
    #[cfg(target_os = "linux")]
    {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        let flags = fcntl(file, FcntlArg::F_GETFL).map_err(std::io::Error::from)?;
        fcntl(
            file,
            FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NOATIME),
        )
        .map_err(std::io::Error::from)?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(invalid("project record read cannot preserve access time"))
    }
}

fn read_project_record_with(
    data_home: &Path,
    id: &str,
    preserve_atime: bool,
) -> Result<PathBuf, DataDirError> {
    let name = record_name(id)?;
    let read = || -> Result<PathBuf, DataDirError> {
        let home = ConfinedDir::open(data_home)?;
        private_dir(&home)?;
        let projects = home.open_dir("projects")?;
        private_dir(&projects)?;
        let mut bytes = Vec::new();
        let file = projects.open_file(&name, FileAccess::Untrusted)?;
        if preserve_atime {
            // Opening and fstat do not read bytes. Set the flag on this same
            // checked descriptor before the first read, retaining confinement.
            preserve_access_time(&file)?;
        }
        file.take(4097).read_to_end(&mut bytes)?;
        parse_record(&bytes)
    };
    read().map_err(|e| invalid(format!("project record projects/{name}: {e}")))
}

/// Publish once. A record already at the hash is accepted only for the same
/// directory; it is never truncated, chmodded or followed through a link.
pub fn write_project_record(
    data_home: &Path,
    id: &str,
    data_dir: &Path,
) -> Result<(), DataDirError> {
    let name = record_name(id)?;
    let text = format!(
        "{}\n",
        data_dir
            .to_str()
            .ok_or_else(|| invalid("data directory is not UTF-8"))?
    );
    parse_record(text.as_bytes())?;
    let home = match ConfinedDir::create(data_home) {
        Ok(root) => root,
        Err(ConfinedError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            ConfinedDir::open(data_home)?
        }
        Err(e) => return Err(e.into()),
    };
    private_dir(&home)?;
    let projects = home.ensure_dir("projects")?;
    private_dir(&projects)?;
    match projects.create_file(&name) {
        Ok(mut file) => {
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            projects.sync()?;
            home.sync()?;
            Ok(())
        }
        Err(ConfinedError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_project_record(data_home, id)? == data_dir {
                Ok(())
            } else {
                Err(invalid(format!(
                    "project record projects/{name} already names another directory"
                )))
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// The caller supplies environment choices. This module never reads a home or
/// creates state merely to resolve where an existing project lives.
pub fn resolve(
    data_dir: Option<&Path>,
    data_home: &Path,
    id: &str,
) -> Result<PathBuf, DataDirError> {
    if let Some(path) = data_dir {
        if !path.is_absolute() {
            return Err(invalid("ORCH_DATA_DIR must be absolute"));
        }
        return Ok(path.to_owned());
    }
    read_project_record(data_home, id)
}
