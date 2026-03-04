//! Pipeline orchestration — frame sampling, preprocessing, inference,
//! postprocessing, and annotation.

pub mod sampler;

#[cfg(feature = "engine")]
pub mod annotator;
#[cfg(feature = "engine")]
pub mod compiled;
#[cfg(feature = "engine")]
pub mod context;
#[cfg(feature = "engine")]
pub(crate) mod defaults;
#[cfg(feature = "engine")]
pub mod postprocess;
#[cfg(feature = "engine")]
pub mod preprocess;
#[cfg(feature = "engine")]
pub mod registry;
#[cfg(feature = "engine")]
pub mod roi;
#[cfg(feature = "engine")]
pub mod tracker;
