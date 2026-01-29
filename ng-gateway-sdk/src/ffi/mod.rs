//! Internal FFI/runtime helpers used by macro expansions.
//!
//! # Notes
//! - This module is **not** part of the public SDK surface.
//! - It exists to keep `ng_driver_factory!` / `ng_plugin_factory!` expansions small and readable.

pub mod runtime_aware;

pub use runtime_aware::{RuntimeAwareDriverFactory, RuntimeAwarePluginFactory};
