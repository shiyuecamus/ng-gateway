//! Pipeline orchestration — frame sampling, preprocessing, inference,
//! postprocessing, and annotation.

pub mod sampler;

#[cfg(feature = "engine")]
pub mod annotator;
#[cfg(feature = "engine")]
pub mod context;
#[cfg(feature = "engine")]
pub mod postprocess;
#[cfg(feature = "engine")]
pub mod preprocess;
#[cfg(feature = "engine")]
pub mod roi;
