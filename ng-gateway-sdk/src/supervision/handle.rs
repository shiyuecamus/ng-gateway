//! Lock-free publication of a "ready" data-plane handle.
//!
//! # Design goals
//! - **Lock-free hot read**: data-plane methods load the latest handle without locks.
//! - **Immediate invalidation**: on disconnect/failure the handle is cleared.
//! - **Cheap clone**: `Arc` is used as the handle carrier.

use arc_swap::ArcSwapOption;
use std::sync::Arc;

/// A lock-free cell holding an optional `Arc` handle.
///
/// # Performance
/// - `load()` is lock-free and cheap; it performs an atomic load and clones the `Arc`.
/// - `store()` is lock-free and performs an atomic swap.
pub struct HandleCell<H> {
    inner: ArcSwapOption<H>,
}

impl<H> Default for HandleCell<H> {
    fn default() -> Self {
        Self {
            inner: ArcSwapOption::from(None),
        }
    }
}

impl<H> HandleCell<H> {
    /// Create an empty handle cell.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a new handle (or clear the handle by passing `None`).
    #[inline]
    pub fn store(&self, h: Option<Arc<H>>) {
        self.inner.store(h);
    }

    /// Load the current handle if available.
    #[inline]
    pub fn load(&self) -> Option<Arc<H>> {
        self.inner.load_full()
    }
}
