//! Collection concurrency capability description for southward drivers.
//!
//! The gateway Collector needs a driver-provided, **allocation-free** description of how much
//! concurrency is safe and useful. This is not about "how many futures can we poll", but
//! about protocol semantics:
//!
//! - Some transports (e.g., Modbus RTU on a shared serial bus) must be strictly serialized.
//! - Some drivers own multiple independent I/O lanes (e.g., TCP connection pool) and can safely
//!   run multiple collect calls concurrently.
//!
//! # Design philosophy
//!
//! The Collector does **not** control intra-group parallelism — that is the driver's
//! responsibility inside `collect_data()`.  This profile only describes **cross-group**
//! concurrency: how many independent `GroupCall`s can be in flight at the same time.

use core::num::NonZeroUsize;

/// A driver-provided concurrency capability that the Collector uses to automatically
/// adapt scheduling.
///
/// # Examples
///
/// | Protocol          | Typical value        | Reason                                    |
/// |-------------------|----------------------|-------------------------------------------|
/// | Modbus RTU        | `serial()`           | Shared serial bus, strictly one call       |
/// | Modbus TCP (n=4)  | `concurrent(4)`      | 4 TCP connections = 4 groups in parallel   |
/// | S7 (single conn)  | `serial()`           | One PLC connection, one call at a time     |
/// | EtherNet/IP (n=4) | `concurrent(4)`      | Connection pool size                       |
///
/// # Contract
/// - `max_concurrency` is guaranteed >= 1.
/// - This type is `Copy` and must be cheap to return on hot paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectorConcurrencyProfile {
    /// Maximum number of concurrent in-flight `collect_data()` calls for this
    /// driver instance.
    ///
    /// The Collector uses this to bound `buffer_unordered` and semaphore permits.
    /// A value of 1 means strictly serialized collection.
    pub max_concurrency: NonZeroUsize,
}

impl CollectorConcurrencyProfile {
    /// Strictly serialized profile: one `collect_data()` call at a time.
    ///
    /// This is the safe default for serial-bus protocols (Modbus RTU, etc.)
    /// and single-connection drivers.
    #[inline]
    pub const fn serial() -> Self {
        // SAFETY: literal 1 is non-zero.
        Self {
            max_concurrency: unsafe { NonZeroUsize::new_unchecked(1) },
        }
    }

    /// Concurrent profile: up to `n` in-flight `collect_data()` calls.
    ///
    /// Typically set to the I/O pool size (e.g., TCP connection count).
    /// Falls back to 1 if `n` is 0.
    #[inline]
    pub fn concurrent(n: usize) -> Self {
        let n = n.max(1);
        Self {
            // SAFETY: n >= 1 after `max(1)`.
            max_concurrency: unsafe { NonZeroUsize::new_unchecked(n) },
        }
    }

    /// Returns the maximum concurrency as a plain `usize`.
    #[inline]
    pub const fn get(&self) -> usize {
        self.max_concurrency.get()
    }

    /// Returns `true` if this profile is strictly serialized (`max_concurrency == 1`).
    #[inline]
    pub const fn is_serial(&self) -> bool {
        self.max_concurrency.get() == 1
    }
}
