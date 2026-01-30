//! Southward internal helpers.
//!
//! This module contains small, reusable helpers shared by multiple southward submodules.
//! It is intentionally kept minimal to avoid turning into a "dumping ground".

use ng_gateway_sdk::{RuntimeAction, RuntimePoint};
use std::{
    cell::RefCell,
    sync::{Arc, OnceLock},
    time::Instant,
};

/// Monotonic millisecond tick base for snapshot TTL bookkeeping.
///
/// We store per-point timestamps as `u64` ms since this base to:
/// - reduce per-entry size vs `Instant`
/// - avoid system clock jumps
/// - keep TTL checks as cheap integer math
static SNAPSHOT_TICK_BASE: OnceLock<Instant> = OnceLock::new();

/// Return monotonic milliseconds since `SNAPSHOT_TICK_BASE`.
///
/// # Notes
/// - This is a fast helper intended for hot paths (change detection, TTL refresh).
/// - The returned value is relative time; it MUST NOT be used as wall-clock timestamp.
#[inline]
pub(crate) fn snapshot_now_ms() -> u64 {
    let base = SNAPSHOT_TICK_BASE.get_or_init(Instant::now);
    // Saturating cast is safe for practical uptimes.
    base.elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// Build a reverse-lookup key for `(channel_name, device_name, point_key)`.
///
/// # Notes
/// This is NOT a hot-path function. Telemetry encoding should prefer `point_id -> PointMeta`.
#[inline]
pub(crate) fn make_point_path_key(
    channel_name: &str,
    device_name: &str,
    point_key: &str,
) -> String {
    // Use a low-likelihood separator to avoid ambiguity while keeping the key compact.
    const SEP: char = '\u{1f}';
    let mut s = String::with_capacity(channel_name.len() + device_name.len() + point_key.len() + 2);
    s.push_str(channel_name);
    s.push(SEP);
    s.push_str(device_name);
    s.push(SEP);
    s.push_str(point_key);
    s
}

thread_local! {
    /// Thread-local buffer used to build point reverse-lookup keys without allocations.
    ///
    /// # Why thread-local?
    /// - `DashMap<String, _>` can be queried by `&str` (via `Borrow<str>`), so we only need a
    ///   temporary string slice for lookup.
    /// - Using a per-thread reusable `String` avoids per-call heap allocations on hot paths
    ///   (write-back/topic-based routing).
    static POINT_PATH_KEY_BUF: RefCell<String> = RefCell::new(String::with_capacity(128));
}

/// Build a point reverse-lookup key into a thread-local buffer and pass it to `f`.
///
/// # Safety & usage
/// - The `&str` passed to `f` is only valid for the duration of the call.
/// - `f` MUST NOT `.await` or store the reference.
#[inline]
pub(crate) fn with_point_path_key<R>(
    channel_name: &str,
    device_name: &str,
    point_key: &str,
    f: impl FnOnce(&str) -> R,
) -> R {
    const SEP: char = '\u{1f}';
    POINT_PATH_KEY_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        buf.clear();
        buf.reserve(channel_name.len() + device_name.len() + point_key.len() + 2);
        buf.push_str(channel_name);
        buf.push(SEP);
        buf.push_str(device_name);
        buf.push(SEP);
        buf.push_str(point_key);
        f(buf.as_str())
    })
}

/// Return a cached empty runtime point slice.
///
/// # Performance
/// This avoids allocating a new empty `Vec`/`Arc<[..]>` on repeated "no points" lookups,
/// which can become a hot path in read-heavy scenarios.
#[inline]
pub(crate) fn empty_runtime_points() -> Arc<[Arc<dyn RuntimePoint>]> {
    static EMPTY: OnceLock<Arc<[Arc<dyn RuntimePoint>]>> = OnceLock::new();
    Arc::clone(
        EMPTY.get_or_init(|| Arc::from(Vec::<Arc<dyn RuntimePoint>>::new().into_boxed_slice())),
    )
}

/// Return a cached empty runtime action slice.
///
/// # Performance
/// This avoids allocating a new empty `Vec`/`Arc<[..]>` on repeated "no actions" lookups.
#[inline]
pub(crate) fn empty_runtime_actions() -> Arc<[Arc<dyn RuntimeAction>]> {
    static EMPTY: OnceLock<Arc<[Arc<dyn RuntimeAction>]>> = OnceLock::new();
    Arc::clone(
        EMPTY.get_or_init(|| Arc::from(Vec::<Arc<dyn RuntimeAction>>::new().into_boxed_slice())),
    )
}
