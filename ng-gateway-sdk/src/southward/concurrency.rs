//! Collection concurrency capability description for southward drivers.
//!
//! The gateway Collector needs a driver-provided, **allocation-free** description of how much
//! concurrency is safe and useful. This is not only about "how many futures can we poll", but
//! about protocol semantics:
//! - Some transports (e.g., Modbus RTU on a shared serial bus) must be strictly serialized.
//! - Some drivers own multiple independent I/O lanes (e.g., TCP connection pool) and can safely
//!   run multiple collect calls concurrently.
//!
//! This module provides `CollectorConcurrencyProfile` for expressing:
//! - **cross-group concurrency** (global in-flight `collect_data()` calls)
//! - **intra-group-key concurrency** (max in-flight calls that share the same `CollectionGroupKey`)
//! - **I/O lane count** (informational / sizing hint, typically equals pool size)

use core::num::NonZeroUsize;

/// A driver-provided capability profile that the Collector uses to automatically adapt concurrency.
///
/// # Field meanings
/// - `global_max_inflight`: Maximum number of concurrent in-flight `collect_data()` calls for this
///   driver instance (cross-group concurrency).
/// - `per_group_key_max_inflight`: Maximum number of concurrent in-flight `collect_data()` calls
///   that share the same physical `CollectionGroupKey` (intra-group-key concurrency).
/// - `io_lanes`: Number of independent I/O lanes owned by the driver (e.g. TCP pool size).
///   This is a sizing hint and may be equal to `global_max_inflight` for most drivers.
///
/// # Contracts
/// - All values are guaranteed to be >= 1.
/// - This type is `Copy` and must be cheap to return on hot paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectorConcurrencyProfile {
    pub global_max_inflight: NonZeroUsize,
    pub per_group_key_max_inflight: NonZeroUsize,
    pub io_lanes: NonZeroUsize,
}

impl CollectorConcurrencyProfile {
    /// Strictly serialized profile (safe default).
    #[inline]
    pub fn serial() -> Self {
        // SAFETY: literal 1 is non-zero.
        let one = unsafe { NonZeroUsize::new_unchecked(1) };
        Self {
            global_max_inflight: one,
            per_group_key_max_inflight: one,
            io_lanes: one,
        }
    }

    /// Create a profile from a known I/O lane count.
    ///
    /// This sets:
    /// - `io_lanes = lanes`
    /// - `global_max_inflight = lanes`
    /// - `per_group_key_max_inflight = 1` (safe default)
    #[inline]
    pub fn from_io_lanes(lanes: usize) -> Self {
        let lanes = lanes.max(1);
        // SAFETY: lanes is >= 1 after `max(1)`.
        let lanes = NonZeroUsize::new(lanes).unwrap();
        // SAFETY: literal 1 is non-zero.
        let one = unsafe { NonZeroUsize::new_unchecked(1) };
        Self {
            global_max_inflight: lanes,
            per_group_key_max_inflight: one,
            io_lanes: lanes,
        }
    }

    /// Override the global max in-flight value (must be >= 1).
    #[inline]
    pub fn with_global_max_inflight(mut self, v: usize) -> Self {
        let v = NonZeroUsize::new(v.max(1)).unwrap_or(self.global_max_inflight);
        self.global_max_inflight = v;
        self
    }

    /// Override per-group-key max in-flight value (must be >= 1).
    #[inline]
    pub fn with_per_group_key_max_inflight(mut self, v: usize) -> Self {
        let v = NonZeroUsize::new(v.max(1)).unwrap_or(self.per_group_key_max_inflight);
        self.per_group_key_max_inflight = v;
        self
    }

    /// Override I/O lane count (must be >= 1).
    #[inline]
    pub fn with_io_lanes(mut self, v: usize) -> Self {
        let v = NonZeroUsize::new(v.max(1)).unwrap_or(self.io_lanes);
        self.io_lanes = v;
        self
    }
}
