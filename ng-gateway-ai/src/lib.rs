//! # ng-gateway-ai — AI Processing Engine
//!
//! This crate provides AI vision analysis capabilities for the ng-gateway
//! IoT platform. It supports:
//!
//! - **Model management**: ONNX model registry with lazy loading and metadata extraction
//! - **Inference scheduling**: Worker pool with backpressure and concurrency control
//! - **Pipeline orchestration**: Multi-stage processing (decode → preprocess → infer → postprocess)
//! - **Built-in processors**: YOLOv8/v5 detection, classification, segmentation postprocessing;
//!   Letterbox/CenterCrop/DirectResize preprocessing with configurable normalization
//! - **Alarm evaluation**: Rule-based alarm generation from analysis results
//! - **Frame annotation**: Bounding box and label rendering to JPEG snapshots
//!
//! # Feature Flags
//!
//! - **default** (no features): Re-exports API types and traits via [`api`].
//!   Camera drivers (cdylib) should depend only on `ng-gateway-sdk` + `ng-gateway-ai`,
//!   and use `ng_gateway_ai::api::*` — no direct dependency on `ng-gateway-models` or
//!   `ng-gateway-error`.
//!
//! - **`engine`**: Full engine implementation including ONNX Runtime, inference pool,
//!   model registry, pre/post processors, frame decoder, and metrics. Only the
//!   host process (`ng-gateway-core`) should enable this feature.

#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

// ── Driver-facing API re-exports ────────────────────────────────────
//
// Drivers MUST NOT directly depend on ng-gateway-models or ng-gateway-error.
// All AI-related types are re-exported here to maintain a single dependency
// boundary and ensure API stability when domain models evolve.

/// Re-exports of AI domain types for southward driver consumption.
///
/// Use `use ng_gateway_ai::api::*` or `use ng_gateway_ai::api::{AiEngineApi, FrameFormat, ...}`.
/// Do not depend on `ng-gateway-models` or `ng-gateway-error` in drivers.
pub mod api {
    pub use ng_gateway_error::ai::{AiEngineError, AiResult};
    pub use ng_gateway_models::{
        domain::prelude::{
            AlarmEvent, AnalysisResult, ChannelRegistration, FrameAnalysisRequest, NewPipeline,
            PipelineInfo, StreamTransport, VideoFrame,
        },
        entities::ai::pipeline::RoiRegions,
        enums::ai::{AlarmSeverity, FrameFormat, SamplingStrategy},
        AiAlgorithmRegistry, AiEngineApi, AiInferenceRuntime, AiModelRegistry, AiPipelineRegistry,
    };
}

// ── Internal implementation modules ────────────────────────────────

pub mod frame;
pub mod inference;
pub mod model;
pub mod pipeline;
pub mod result;

/// WASM algorithm host runtime (internal implementation).
pub mod algorithm;

#[cfg(feature = "engine")]
pub mod engine;

// ── Engine-internal decoded frame type ─────────────────────────────

mod decoded;
pub use decoded::DecodedFrame;

#[cfg(test)]
mod test_utils;

// Engine-only re-exports
#[cfg(feature = "engine")]
pub use engine::AiEngine;
