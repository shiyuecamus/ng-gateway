//! Log file cleanup worker (retention + max_files guard).
//!
//! This module provides:
//! - A reusable "run once" cleanup function (supports dry-run preview)
//! - A background worker that periodically enforces retention policies
//!
//! # Safety
//! We must **never** delete active log files that are currently being written, because on Unix
//! deleting an open file will keep the inode alive and the process will keep writing to a
//! now-unlinked file (operators lose the file path).
//!
//! We protect active files by:
//! - never deleting files whose `mtime` is unknown (`modified_at_ms == 0`)
//! - never deleting files modified within a recent grace window

use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::settings::Settings;
use ng_gateway_utils::log_files::{self, LogFileMeta};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Default "active file" grace window to avoid deleting currently written files.
const ACTIVE_GRACE_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct CleanupReport {
    pub deleted: Vec<LogFileMeta>,
    pub freed_bytes: u64,
    pub protected_active: Vec<String>,
}

#[inline]
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn compute_protected(files: &[LogFileMeta], now_ms: i64) -> HashSet<String> {
    let mut protected: HashSet<String> = HashSet::new();
    for f in files {
        if f.modified_at_ms == 0 {
            protected.insert(f.name.clone());
            continue;
        }
        if f.modified_at_ms >= now_ms.saturating_sub(ACTIVE_GRACE_MS) {
            protected.insert(f.name.clone());
        }
    }
    protected
}

fn plan_cleanup(
    mut files: Vec<LogFileMeta>,
    protected: &HashSet<String>,
    now_ms: i64,
    max_days: u32,
    max_total_size_mb: u64,
    max_files: usize,
) -> (Vec<LogFileMeta>, Vec<String>) {
    // Sort oldest first for deterministic deletion decisions.
    files.sort_by(|a, b| {
        a.modified_at_ms
            .cmp(&b.modified_at_ms)
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut to_delete: Vec<LogFileMeta> = Vec::new();
    let mut protected_hits: HashSet<String> = HashSet::new();

    // Phase 1: time-based retention.
    if max_days > 0 {
        let cutoff_ms = now_ms.saturating_sub(max_days as i64 * 24 * 60 * 60 * 1000);
        for f in files.iter() {
            if f.modified_at_ms != 0 && f.modified_at_ms < cutoff_ms {
                if protected.contains(&f.name) {
                    protected_hits.insert(f.name.clone());
                    continue;
                }
                to_delete.push(f.clone());
            }
        }
    }

    // Remaining set after time-based deletions (best-effort).
    let mut remaining: Vec<LogFileMeta> = files
        .into_iter()
        .filter(|f| !to_delete.iter().any(|d| d.name == f.name))
        .collect();
    remaining.sort_by(|a, b| {
        a.modified_at_ms
            .cmp(&b.modified_at_ms)
            .then_with(|| a.name.cmp(&b.name))
    });

    // Phase 2: max_files guard (0 is treated as "no guard" here).
    if max_files > 0 {
        while remaining.len() > max_files {
            let Some(oldest) = remaining.first().cloned() else {
                break;
            };
            if protected.contains(&oldest.name) {
                protected_hits.insert(oldest.name.clone());
                // Cannot delete the oldest; drop it from consideration and keep it.
                // This may prevent reaching the guard target; that's an acceptable safety tradeoff.
                remaining.remove(0);
                continue;
            }
            to_delete.push(oldest.clone());
            remaining.remove(0);
        }
    }

    // Phase 3: size-based retention (0 means unlimited).
    if max_total_size_mb > 0 {
        let max_bytes = max_total_size_mb.saturating_mul(1024 * 1024);
        let mut total: u64 = remaining.iter().map(|f| f.size).sum();
        while total > max_bytes {
            let Some(oldest) = remaining.first().cloned() else {
                break;
            };
            if protected.contains(&oldest.name) {
                protected_hits.insert(oldest.name.clone());
                remaining.remove(0);
                continue;
            }
            total = total.saturating_sub(oldest.size);
            to_delete.push(oldest.clone());
            remaining.remove(0);
        }
    }

    // Dedup deletions by name (stable).
    to_delete.sort_by(|a, b| {
        a.modified_at_ms
            .cmp(&b.modified_at_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    to_delete.dedup_by(|a, b| a.name == b.name);

    let mut protected_active: Vec<String> = protected_hits.into_iter().collect();
    protected_active.sort();
    (to_delete, protected_active)
}

pub fn cleanup_logs_once(settings: &Settings, dry_run: bool) -> NGResult<CleanupReport> {
    let output = settings.logging.output.get();
    let dir = output.file.dir.trim();
    if dir.is_empty() {
        return Err(NGError::from("logging.output.file.dir cannot be empty"));
    }

    let log_dir = PathBuf::from(dir);
    let scan = log_files::scan_log_dir(&log_dir)
        .map_err(|e| NGError::from(format!("Failed to scan log dir {}: {e}", log_dir.display())))?;

    let now = now_ms();
    let protected = compute_protected(&scan.files, now);

    let (to_delete, protected_active) = plan_cleanup(
        scan.files,
        &protected,
        now,
        output.file.retention.max_days,
        output.file.retention.max_total_size_mb,
        output.file.rotation.max_files,
    );

    if dry_run {
        let freed_bytes: u64 = to_delete.iter().map(|f| f.size).sum();
        return Ok(CleanupReport {
            deleted: to_delete,
            freed_bytes,
            protected_active,
        });
    }

    let mut freed_bytes: u64 = 0;
    let mut deleted: Vec<LogFileMeta> = Vec::new();
    for f in to_delete.into_iter() {
        // Extra safety: only allow safe file names.
        log_files::validate_safe_file_name(&f.name).map_err(|e| NGError::from(e.to_string()))?;
        let path = Path::new(&log_dir).join(&f.name);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                freed_bytes = freed_bytes.saturating_add(f.size);
                deleted.push(f);
            }
            Err(e) => {
                // Best-effort: keep going; report partial progress.
                tracing::warn!(error=%e, file=%path.display(), "Failed to delete log file");
            }
        }
    }

    Ok(CleanupReport {
        deleted,
        freed_bytes,
        protected_active,
    })
}

pub fn spawn_cleanup_worker(settings: Settings, shutdown: CancellationToken) {
    tokio::spawn(async move {
        loop {
            if shutdown.is_cancelled() {
                break;
            }

            let cleanup = settings.logging.cleanup.get();
            let interval_ms = cleanup.interval_ms.max(200);
            if cleanup.enabled {
                let _ = cleanup_logs_once(&settings, false);
            }

            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {},
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, size: u64, modified_at_ms: i64) -> LogFileMeta {
        LogFileMeta {
            name: name.to_string(),
            size,
            modified_at_ms,
        }
    }

    #[test]
    fn cleanup_respects_protected_active_files() {
        let now = 1_700_000_000_000i64;
        let files = vec![
            meta("old.log.1", 10, now - 10 * 24 * 60 * 60 * 1000),
            meta("active.log", 999, now - 30_000),
            meta("unknown.log", 123, 0),
        ];
        let protected = compute_protected(&files, now);
        assert!(protected.contains("active.log"));
        assert!(protected.contains("unknown.log"));

        let (del, protected_hits) = plan_cleanup(files, &protected, now, 7, 0, 0);
        assert!(del.iter().any(|f| f.name == "old.log.1"));
        assert!(!del.iter().any(|f| f.name == "active.log"));
        // `protected_hits` only records protected files that would have been deleted by policy.
        // Since `active.log` is not selected by the max_days policy, it may not appear here.
        let _ = protected_hits;
    }

    #[test]
    fn cleanup_enforces_max_files_and_size() {
        let now = 1_700_000_000_000i64;
        let files = vec![
            meta("a.log", 10 * 1024 * 1024, now - 300_000),
            meta("b.log", 10 * 1024 * 1024, now - 200_000),
            meta("c.log", 10 * 1024 * 1024, now - 100_000),
        ];
        let protected: HashSet<String> = HashSet::new();

        // Keep max 2 files.
        let (del, _) = plan_cleanup(files.clone(), &protected, now, 0, 0, 2);
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].name, "a.log");

        // Keep total size <= 15MB => delete oldest until within budget.
        let (del, _) = plan_cleanup(files, &protected, now, 0, 15, 0);
        assert_eq!(del.len(), 2);
        assert_eq!(del[0].name, "a.log");
        assert_eq!(del[1].name, "b.log");
    }
}
