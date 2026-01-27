//! Log directory utilities (server-side).
//!
//! This module provides small, reusable helpers for:
//! - scanning the runtime `logs/` directory
//! - validating log file names to prevent path traversal / ZipSlip
//!
//! # Design goals
//! - **Zero business logic**: only filesystem + validation helpers.
//! - **Safe defaults**: deny suspicious file names.
//! - **Portable**: no Actix/Web types; return `std::io` errors.

use std::{
    collections::HashMap,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

/// A single log file entry discovered under a log directory.
#[derive(Debug, Clone)]
pub struct LogFileMeta {
    /// File name (base name only, no directory component).
    pub name: String,
    /// File size (bytes).
    pub size: u64,
    /// Modified time in milliseconds since UNIX epoch (best-effort, may be 0).
    pub modified_at_ms: i64,
}

/// Scan result of a log directory.
#[derive(Debug, Clone, Default)]
pub struct LogDirScan {
    /// Discovered files.
    pub files: Vec<LogFileMeta>,
}

/// Scan a log directory and return discovered log files.
///
/// # Daily naming support
/// `tracing-appender` daily uses the pattern:
/// - `host.log.<date>` / `<driver>.log.<date>`
///
/// This function treats any file that contains `.log` as a candidate.
///
/// # Cold start
/// If `log_dir` does not exist, returns an empty result.
pub fn scan_log_dir(log_dir: &Path) -> io::Result<LogDirScan> {
    let mut out = LogDirScan::default();

    let entries = match std::fs::read_dir(log_dir) {
        Ok(v) => v,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };

    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        if !looks_like_log_name(&name) {
            continue;
        }
        // Only expose safe basenames; callers can still show "unsafe" files via other means,
        // but we default to strict here.
        if validate_safe_file_name(&name).is_err() {
            continue;
        }

        let Ok(meta) = entry.metadata() else { continue };
        let modified_at_ms = meta
            .modified()
            .ok()
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as i64)
            })
            .unwrap_or(0);

        out.files.push(LogFileMeta {
            name,
            size: meta.len(),
            modified_at_ms,
        });
    }
    Ok(out)
}

/// Build a `name -> absolute path` map of allowed files under the log directory.
///
/// This is intended for server-side download endpoints:
/// - only files discovered from the directory are allowed
/// - only safe file names are included
pub fn build_allowed_map(log_dir: &Path) -> io::Result<HashMap<String, PathBuf>> {
    let mut m: HashMap<String, PathBuf> = HashMap::new();
    let entries = match std::fs::read_dir(log_dir) {
        Ok(v) => v,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(m),
        Err(e) => return Err(e),
    };

    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        if !looks_like_log_name(&name) {
            continue;
        }
        if validate_safe_file_name(&name).is_ok() {
            m.insert(name, entry.path());
        }
    }

    Ok(m)
}

/// Whether a file name looks like our log file naming convention.
#[inline]
pub fn looks_like_log_name(name: &str) -> bool {
    name.contains(".log")
}

/// Whether a file name is a host log (daily or non-daily).
#[inline]
pub fn is_host_log_name(name: &str) -> bool {
    name == "host.log" || name.starts_with("host.log.")
}

/// Validate a user-provided file name as a safe base name.
///
/// # Security
/// Deny:
/// - empty names
/// - path separators
/// - `..` traversal
/// - NUL bytes
/// - hidden files (starts with `.`)
pub fn validate_safe_file_name(name: &str) -> io::Result<()> {
    if name.is_empty() {
        return Err(io::Error::new(ErrorKind::InvalidInput, "empty file name"));
    }
    if name.contains('\0')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("invalid file name: {name}"),
        ));
    }
    Ok(())
}
