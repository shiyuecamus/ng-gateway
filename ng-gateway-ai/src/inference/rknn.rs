//! RKNN inference backend — NPU-accelerated inference for Rockchip platforms.
//!
//! Built on the [`rknpu2`](https://docs.rs/rknpu2) crate (safe Rust bindings
//! for `librknnrt.so`, aligned with RKNN Toolkit2 SDK v2.3.2).
//!
//! # Key Characteristics
//!
//! - **Input**: NHWC uint8 for quantized models (no float32 normalization needed)
//! - **Zero-copy**: DMA-buf fd import via `RKNNAPI::create_mem_from_fd()` (unsafe FFI)
//! - **Multi-core**: `set_core_mask()` for load balancing across NPU cores
//! - **Output**: INT8/UINT8 requiring dequantization: `float = (raw - zp) * scale`
//!
//! # Feature Gates
//!
//! This module is compiled only when the `rknn` feature is enabled. The `libloading`
//! feature of rknpu2 enables runtime loading of `librknnrt.so`.

#[cfg(all(feature = "rknn", target_os = "linux", target_arch = "aarch64"))]
mod inner {
    use crate::{
        frame::memory::FrameMemory,
        inference::{backend::InferTiming, backend::ModelBackend, RawInferenceOutput},
        pipeline::preprocess::PreprocessOutput,
    };
    use dashmap::DashMap;
    use ndarray::ArrayD;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::enums::ai::ModelFormat;
    use rknpu2::{
        api::RknnInitFlags,
        io::{
            buffer::BufView,
            input::Input,
            output::{Output, OutputKind},
        },
        query::{
            input_attr::InputAttr, output_attr::OutputAttr, InputOutputNum, Query, QueryWithInput,
            TensorAttrView,
        },
        rknn::{NpuCores, RKNN},
        tensor::{DataTypeKind, QuantTypeKind, TensorFormatKind},
    };
    #[cfg(feature = "dmabuf")]
    use std::os::unix::io::AsRawFd;
    use std::{path::Path, sync::Arc, time::Instant};
    use tracing::{debug, info, warn};

    /// Default library path for RKNN runtime shared object.
    const RKNN_LIBRARY_PATH: &str = "librknnrt.so";

    /// RKNN inference backend for Rockchip NPU.
    ///
    /// Each loaded model owns an RKNN context behind a `parking_lot::Mutex`.
    /// The mutex hold time is bounded to a single `rknn_run()` call (~5-20ms).
    pub struct RknnBackend {
        /// Loaded model contexts keyed by model id.
        models: DashMap<i32, Arc<LoadedRknnModel>>,
        /// NPU core mask for `set_core_mask()`.
        core_mask: u32,
    }

    /// A loaded RKNN model with pre-queried tensor attributes.
    struct LoadedRknnModel {
        /// RKNN context (behind a mutex for thread-safe inference).
        ctx: parking_lot::Mutex<RKNN<rknpu2::api::runtime::RuntimeAPI>>,
        /// Pre-queried input tensor attributes.
        input_attrs: Vec<InputAttr>,
        /// Pre-queried output tensor attributes (for dequantization params).
        output_attrs: Vec<OutputAttr>,
        /// Model file path (for diagnostics).
        #[allow(dead_code)]
        path: std::path::PathBuf,
        /// Estimated model memory consumption.
        estimated_bytes: u64,
    }

    // SAFETY: RKNN context access is serialized via `parking_lot::Mutex`.
    // The raw context handle is an opaque integer, safe to send across threads.
    unsafe impl Send for LoadedRknnModel {}
    unsafe impl Sync for LoadedRknnModel {}

    impl RknnBackend {
        /// Create a new RKNN backend with the given NPU core mask.
        ///
        /// Core mask values for RK3588 (3 NPU cores):
        /// - `0` — auto (runtime decides)
        /// - `1` — core 0, `2` — core 1, `4` — core 2
        /// - `7` — all three cores
        pub fn new(core_mask: u32) -> Self {
            Self {
                models: DashMap::new(),
                core_mask,
            }
        }
    }

    impl Default for RknnBackend {
        fn default() -> Self {
            Self::new(0)
        }
    }

    #[async_trait::async_trait]
    impl ModelBackend for RknnBackend {
        fn format(&self) -> ModelFormat {
            ModelFormat::Rknn
        }

        fn supports_dma_input(&self) -> bool {
            true
        }

        async fn load(&self, model_id: i32, path: &Path) -> Result<(), AiEngineError> {
            if self.models.contains_key(&model_id) {
                return Ok(());
            }

            let path_buf = path.to_path_buf();
            let core_mask = self.core_mask;

            let loaded = tokio::task::spawn_blocking(move || load_rknn_model(&path_buf, core_mask))
                .await
                .map_err(|e| AiEngineError::ModelLoadError(format!("join error: {e}")))??;

            info!(
                model_id,
                path = %path.display(),
                inputs = loaded.input_attrs.len(),
                outputs = loaded.output_attrs.len(),
                "RKNN model loaded onto NPU"
            );

            self.models.insert(model_id, Arc::new(loaded));
            Ok(())
        }

        fn unload(&self, model_id: i32) {
            self.models.remove(&model_id);
            debug!(model_id, "RKNN model unloaded");
        }

        fn is_loaded(&self, model_id: i32) -> bool {
            self.models.contains_key(&model_id)
        }

        async fn infer(
            &self,
            model_id: i32,
            input: PreprocessOutput,
        ) -> Result<(RawInferenceOutput, InferTiming), AiEngineError> {
            let model = self
                .models
                .get(&model_id)
                .map(|m| Arc::clone(m.value()))
                .ok_or(AiEngineError::ModelNotFound(model_id.to_string()))?;

            tokio::task::spawn_blocking(move || run_rknn_inference(&model, input))
                .await
                .map_err(|e| {
                    AiEngineError::InferenceError(format!(
                        "RKNN inference join error for model {model_id}: {e}"
                    ))
                })?
        }

        fn loaded_count(&self) -> usize {
            self.models.len()
        }

        fn estimated_memory_bytes(&self) -> u64 {
            self.models.iter().map(|m| m.value().estimated_bytes).sum()
        }
    }

    /// Load an RKNN model from disk, initialize the context, query tensor
    /// attributes, and configure core mask.
    fn load_rknn_model(path: &Path, core_mask: u32) -> Result<LoadedRknnModel, AiEngineError> {
        let mut model_data = std::fs::read(path)
            .map_err(|e| AiEngineError::ModelLoadError(format!("read RKNN model: {e}")))?;
        let estimated_bytes = model_data.len() as u64;

        let ctx =
            RKNN::new_with_library(RKNN_LIBRARY_PATH, &mut model_data, RknnInitFlags::empty())
                .map_err(|e| {
                    AiEngineError::ModelLoadError(format!(
                        "RKNN init failed (ensure librknnrt.so is on LD_LIBRARY_PATH): {e}"
                    ))
                })?;

        // Configure NPU core mask for multi-core scheduling.
        if core_mask != 0 {
            let cores = NpuCores::from_bits_truncate(core_mask);
            if let Err(e) = ctx.set_core_mask(cores) {
                warn!(
                    core_mask,
                    error = %e,
                    "set_core_mask failed, continuing with default"
                );
            }
        }

        // Query input/output tensor counts.
        let io_num: InputOutputNum = ctx.query().map_err(|e| {
            AiEngineError::ModelLoadError(format!("rknn query IN_OUT_NUM failed: {e}"))
        })?;

        // Query input tensor attributes.
        let mut input_attrs = Vec::with_capacity(io_num.input_num() as usize);
        for i in 0..io_num.input_num() {
            let attr: InputAttr = ctx.query_with_input(i).map_err(|e| {
                AiEngineError::ModelLoadError(format!("rknn query INPUT_ATTR[{i}] failed: {e}"))
            })?;
            input_attrs.push(attr);
        }

        // Query output tensor attributes (contains quantization params).
        let mut output_attrs = Vec::with_capacity(io_num.output_num() as usize);
        for i in 0..io_num.output_num() {
            let attr: OutputAttr = ctx.query_with_input(i).map_err(|e| {
                AiEngineError::ModelLoadError(format!("rknn query OUTPUT_ATTR[{i}] failed: {e}"))
            })?;
            output_attrs.push(attr);
        }

        debug!(
            path = %path.display(),
            inputs = input_attrs.len(),
            outputs = output_attrs.len(),
            core_mask,
            "RKNN model context initialized"
        );

        Ok(LoadedRknnModel {
            ctx: parking_lot::Mutex::new(ctx),
            input_attrs,
            output_attrs,
            path: path.to_path_buf(),
            estimated_bytes,
        })
    }

    /// Execute RKNN inference synchronously (called from `spawn_blocking`).
    ///
    /// Supports two input paths:
    /// 1. **DMA zero-copy** (`DeviceMemory` with `DmaBuf`): Uses the low-level
    ///    `RKNNAPI::create_mem_from_fd()` FFI to import the DMA-buf fd directly
    ///    into NPU memory — no CPU copy. Requires the forked rknpu2 crate with
    ///    exposed DMA-buf API.
    /// 2. **CPU path** (`CpuTensor` or CPU-backed `DeviceMemory`): Converts to
    ///    uint8 NHWC and passes via `set_inputs()` with the high-level API.
    fn run_rknn_inference(
        model: &LoadedRknnModel,
        input: PreprocessOutput,
    ) -> Result<(RawInferenceOutput, InferTiming), AiEngineError> {
        // ── DMA zero-copy fast path ────────────────────────────────
        //
        // When the preprocessor produces a DMA-buf backed DeviceMemory
        // whose dimensions match the model input exactly, we can import
        // the fd directly into NPU memory via RKNN's create_mem_from_fd
        // API, skipping all CPU buffer materialization.
        //
        // This path requires the forked rknpu2 crate that exposes:
        //   - RKNN::create_mem_from_fd(fd, size, offset) → RknnMem
        //   - RKNN::set_io_mem(input_mems, output_mems)
        //   - RKNN::destroy_mem(mem)
        #[cfg(feature = "dmabuf")]
        if let PreprocessOutput::DeviceMemory { ref memory, .. } = input {
            if memory.is_dma_buf() {
                if let Some((borrowed_fd, size, offset)) = memory.dma_fd_info() {
                    let raw_fd = borrowed_fd.as_raw_fd();
                    match run_rknn_dma_inference(model, raw_fd, size, offset) {
                        Ok(result) => return Ok(result),
                        Err(e) => {
                            warn!(
                                %e,
                                "DMA zero-copy inference failed, falling back to CPU path"
                            );
                        }
                    }
                }
            }
        }

        // ── CPU fallback path ──────────────────────────────────────

        let cpu_buffer = prepare_cpu_input_buffer(&input)?;

        let wait_start = Instant::now();
        let ctx = model.ctx.lock();
        let infer_wait = wait_start.elapsed();

        let exec_start = Instant::now();

        let fmt = resolve_input_format(&model.input_attrs);
        let rknn_input = Input {
            index: 0,
            buffer: BufView::U8(&cpu_buffer),
            pass_through: false,
            fmt,
        };

        ctx.set_inputs(rknn_input)
            .map_err(|e| AiEngineError::InferenceError(format!("rknn set_inputs failed: {e}")))?;

        ctx.run()
            .map_err(|e| AiEngineError::InferenceError(format!("rknn_run failed: {e}")))?;

        let raw_output = extract_and_dequantize_outputs(&ctx, &model.output_attrs)?;
        let infer_exec = exec_start.elapsed();

        Ok((
            raw_output,
            InferTiming {
                infer_wait,
                infer_exec,
            },
        ))
    }

    /// DMA zero-copy inference path.
    ///
    /// Imports a DMA-buf fd directly into NPU memory via RKNN's
    /// `create_mem_from_fd` API. The data never touches CPU memory.
    ///
    /// # Required rknpu2 fork
    ///
    /// This function requires the forked rknpu2 crate that exposes:
    /// - `RKNN::create_mem_from_fd(fd, size, offset)` → `RknnMem`
    /// - `RKNN::set_io_mem(input_mems, output_mems)`
    /// - `RKNN::destroy_mem(mem)`
    ///
    /// Until the fork is integrated, this function returns an error
    /// indicating DMA inference is not yet available.
    #[cfg(feature = "dmabuf")]
    fn run_rknn_dma_inference(
        _model: &LoadedRknnModel,
        fd: std::os::unix::io::RawFd,
        size: usize,
        offset: u64,
    ) -> Result<(RawInferenceOutput, InferTiming), AiEngineError> {
        // TODO(rknpu2-fork): Implement DMA zero-copy inference.
        //
        // The implementation flow:
        //   1. let ctx = model.ctx.lock();
        //   2. let input_mem = ctx.create_mem_from_fd(fd, size, offset)?;
        //   3. ctx.set_io_mem(&[input_mem], &output_mems)?;
        //   4. ctx.run()?;
        //   5. let outputs = extract_and_dequantize_outputs(&ctx, &model.output_attrs)?;
        //   6. ctx.destroy_mem(input_mem)?;
        //   7. return Ok((outputs, timing));
        //
        // For now, signal to the caller that DMA inference is pending.
        info!(
            fd,
            size, offset, "DMA zero-copy inference requested — pending rknpu2 fork integration"
        );
        Err(AiEngineError::InferenceError(
            "RKNN DMA zero-copy inference not yet available — \
             requires forked rknpu2 crate with exposed create_mem_from_fd API"
                .into(),
        ))
    }

    /// Resolve the tensor format for RKNN input based on model attributes.
    fn resolve_input_format(input_attrs: &[InputAttr]) -> TensorFormatKind {
        input_attrs
            .first()
            .map(|a| a.format())
            .unwrap_or(TensorFormatKind::NHWC(0))
    }

    /// Prepare a CPU uint8 NHWC buffer from `PreprocessOutput` for RKNN input.
    fn prepare_cpu_input_buffer(input: &PreprocessOutput) -> Result<Vec<u8>, AiEngineError> {
        match input {
            PreprocessOutput::CpuTensor { tensor, .. } => {
                warn!(
                    "RKNN received CpuTensor(f32 NCHW) — converting to uint8 NHWC; \
                     this is suboptimal, consider routing through RKNN-specific preprocessor"
                );
                let shape = tensor.shape();
                let (n, c, h, w) = (shape[0], shape[1], shape[2], shape[3]);
                let mut nhwc = Vec::with_capacity(n * h * w * c);
                for batch in 0..n {
                    for y in 0..h {
                        for x in 0..w {
                            for ch in 0..c {
                                let val = tensor[[batch, ch, y, x]];
                                nhwc.push((val * 255.0).clamp(0.0, 255.0) as u8);
                            }
                        }
                    }
                }
                Ok(nhwc)
            }
            PreprocessOutput::DeviceMemory { memory, .. } => {
                let cpu_bytes = memory.to_cpu().map_err(|e| {
                    AiEngineError::InferenceError(format!(
                        "failed to materialize DeviceMemory to CPU for RKNN: {e}"
                    ))
                })?;
                Ok(cpu_bytes.to_vec())
            }
        }
    }

    /// Extract raw outputs from RKNN with automatic dequantization to float32.
    ///
    /// Uses `Preallocated` output mode with `want_float: true` — the RKNN SDK
    /// performs dequantization internally and writes float32 directly into our
    /// pre-allocated buffer. This avoids the `RknnBuffer` pub(crate) access
    /// limitation of `RuntimeAllocated`.
    fn extract_and_dequantize_outputs(
        ctx: &RKNN<rknpu2::api::runtime::RuntimeAPI>,
        output_attrs: &[OutputAttr],
    ) -> Result<RawInferenceOutput, AiEngineError> {
        let n_outputs = output_attrs.len();

        // Pre-allocate float32 buffers for each output tensor.
        let mut output_buffers: Vec<Vec<f32>> = output_attrs
            .iter()
            .map(|attr| vec![0.0f32; attr.num_elements() as usize])
            .collect();

        // Build Output structs with preallocated BufMutView::F32 references.
        let mut outputs: Vec<Output<'_>> = output_buffers
            .iter_mut()
            .enumerate()
            .map(|(i, buf)| Output {
                index: i as u32,
                kind: OutputKind::Preallocated {
                    buf: rknpu2::io::buffer::BufMutView::F32(buf.as_mut_slice()),
                    want_float: true,
                },
            })
            .collect();

        ctx.get_outputs(&mut outputs)
            .map_err(|e| AiEngineError::InferenceError(format!("rknn get_outputs failed: {e}")))?;

        // Convert pre-filled float buffers into named ndarray tensors.
        let mut tensors = Vec::with_capacity(n_outputs);
        for (attr, float_data) in output_attrs.iter().zip(output_buffers.into_iter()) {
            let name = attr.name();
            let dims: Vec<usize> = attr.dims().iter().map(|&d| d as usize).collect();

            let ndarray_shape = ndarray::IxDyn(&dims);
            let arr = ArrayD::from_shape_vec(ndarray_shape, float_data).map_err(|e| {
                AiEngineError::InferenceError(format!(
                    "output tensor shape mismatch for '{name}': {e}"
                ))
            })?;

            tensors.push((name, arr));
        }

        Ok(RawInferenceOutput { tensors })
    }
}

#[cfg(all(feature = "rknn", target_os = "linux", target_arch = "aarch64"))]
pub use inner::*;

// Stub implementation for non-rknn builds or non-ARM-Linux targets.
#[cfg(not(all(feature = "rknn", target_os = "linux", target_arch = "aarch64")))]
mod stub {
    use crate::{
        inference::{backend::InferTiming, backend::ModelBackend, RawInferenceOutput},
        pipeline::preprocess::PreprocessOutput,
    };
    use dashmap::DashMap;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::enums::ai::ModelFormat;
    use std::path::Path;
    use tracing::info;

    /// RKNN inference backend stub (no-op when the `rknn` feature is disabled).
    pub struct RknnBackend {
        loaded: DashMap<i32, std::path::PathBuf>,
    }

    impl RknnBackend {
        pub fn new(_core_mask: u32) -> Self {
            Self {
                loaded: DashMap::new(),
            }
        }
    }

    impl Default for RknnBackend {
        fn default() -> Self {
            Self::new(0)
        }
    }

    #[async_trait::async_trait]
    impl ModelBackend for RknnBackend {
        fn format(&self) -> ModelFormat {
            ModelFormat::Rknn
        }

        fn supports_dma_input(&self) -> bool {
            false
        }

        async fn load(&self, model_id: i32, path: &Path) -> Result<(), AiEngineError> {
            info!(model_id, path = %path.display(), "RKNN model load (stub — feature `rknn` not enabled)");
            self.loaded.insert(model_id, path.to_path_buf());
            Ok(())
        }

        fn unload(&self, model_id: i32) {
            self.loaded.remove(&model_id);
        }

        fn is_loaded(&self, model_id: i32) -> bool {
            self.loaded.contains_key(&model_id)
        }

        async fn infer(
            &self,
            model_id: i32,
            _input: PreprocessOutput,
        ) -> Result<(RawInferenceOutput, InferTiming), AiEngineError> {
            Err(AiEngineError::InferenceError(format!(
                "RKNN inference for model {model_id} unavailable — \
                 compile with feature `rknn` and deploy on Rockchip hardware"
            )))
        }

        fn loaded_count(&self) -> usize {
            self.loaded.len()
        }

        fn estimated_memory_bytes(&self) -> u64 {
            0
        }
    }
}

#[cfg(not(all(feature = "rknn", target_os = "linux", target_arch = "aarch64")))]
pub use stub::*;
