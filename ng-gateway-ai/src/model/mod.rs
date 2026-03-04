//! AI model management — probing, registry, and processor profiles.

pub mod prober;

#[cfg(feature = "engine")]
pub mod profile;
#[cfg(feature = "engine")]
pub mod registry;
