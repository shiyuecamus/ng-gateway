//! RKNN inference backend — NPU-accelerated inference for Rockchip platforms.
//!
//! Uses the RKNN Toolkit Lite2 SDK to run quantized models on the NPU.
//! Only available on ARM Linux with `librknnrt.so` on the library path.
//!
//! # Key Differences from ONNX Backend
//!
//! - Input is typically NHWC uint8 (no float32 normalization needed)
//! - Supports NPU core affinity via `rknn_set_core_mask()`
//! - Zero-copy input via `rknn_inputs_set()` with pass-through mode
//! - Quantized output (INT8/UINT8) requires dequantization in postprocessing

use super::backend::{InferTiming, ModelBackend};
use crate::{
    decoded::DecodedFrame,
    pipeline::{
        postprocess::RawInferenceOutput,
        preprocess::{CoordinateTransform, PreProcessor, PreprocessInput},
    },
};
use dashmap::DashMap;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::enums::ai::{ModelFormat, TensorDType};
use std::{path::Path, time::Instant};
use tracing::info;

/// RKNN inference backend.
///
/// TODO: Full implementation requires the `rknn-lite` FFI crate.
/// Current implementation is a compilable stub that returns appropriate
/// errors when called, allowing the rest of the system to build and
/// test with RKNN model probing while actual NPU inference is WIP.
pub struct RknnBackend {
    /// Loaded model contexts keyed by model id.
    loaded: DashMap<i32, LoadedRknnModel>,
}

/// Placeholder for a loaded RKNN model context.
#[allow(unused)]
struct LoadedRknnModel {
    path: std::path::PathBuf,
}

impl RknnBackend {
    /// Create a new RKNN backend.
    pub fn new() -> Self {
        Self {
            loaded: DashMap::new(),
        }
    }
}

impl Default for RknnBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ModelBackend for RknnBackend {
    fn format(&self) -> ModelFormat {
        ModelFormat::Rknn
    }

    async fn load(&self, model_id: i32, path: &Path) -> Result<(), AiEngineError> {
        // TODO: Implement with actual rknn_lite FFI:
        //   1. rknn_init(&ctx, model_data, model_size, 0)
        //   2. rknn_set_core_mask(ctx, RKNN_NPU_CORE_AUTO)
        //   3. Store ctx handle in loaded map
        info!(model_id, path = %path.display(), "RKNN model load (stub)");
        self.loaded.insert(
            model_id,
            LoadedRknnModel {
                path: path.to_path_buf(),
            },
        );
        Ok(())
    }

    fn unload(&self, model_id: i32) {
        // TODO: rknn_destroy(ctx)
        self.loaded.remove(&model_id);
    }

    fn is_loaded(&self, model_id: i32) -> bool {
        self.loaded.contains_key(&model_id)
    }

    async fn infer(
        &self,
        model_id: i32,
        frame: &DecodedFrame,
        preprocessor: &dyn PreProcessor,
        input_shape: &[i64],
        input_dtype: TensorDType,
    ) -> Result<(RawInferenceOutput, CoordinateTransform, InferTiming), AiEngineError> {
        if !self.loaded.contains_key(&model_id) {
            return Err(AiEngineError::ModelNotFound(model_id.to_string()));
        }

        // TODO: Implement with actual rknn_lite FFI:
        //   1. Preprocess frame → NHWC uint8 buffer (no float normalization)
        //   2. rknn_inputs_set(ctx, 1, &input)
        //   3. rknn_run(ctx, nullptr)
        //   4. rknn_outputs_get(ctx, output_count, &outputs, nullptr)
        //   5. Dequantize INT8 → float32 using scale/zero_point
        //   6. rknn_outputs_release(ctx, output_count, &outputs)
        //
        // For now, preprocess normally and return an error indicating
        // the RKNN runtime is not yet linked.
        let preprocess_start = Instant::now();
        let _output = preprocessor.process(PreprocessInput {
            frame,
            model_input_shape: input_shape,
            model_input_dtype: input_dtype,
        })?;
        let _preprocess_elapsed = preprocess_start.elapsed();

        Err(AiEngineError::InferenceError(
            "RKNN inference not yet implemented — waiting for rknn-lite FFI crate".to_string(),
        ))
    }

    fn loaded_count(&self) -> usize {
        self.loaded.len()
    }

    fn estimated_memory_bytes(&self) -> u64 {
        0
    }
}
