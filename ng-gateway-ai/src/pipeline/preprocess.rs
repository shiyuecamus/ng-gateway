//! Preprocessing trait and built-in implementations.
//!
//! Engine-internal module: NOT exposed to users or drivers.
//! Users control preprocessing via `PreProcessorConfig` parameters in
//! the Pipeline API; the engine maps those to the correct implementation.

#[cfg(feature = "engine")]
mod inner {
    use crate::{
        decoded::DecodedFrame,
        frame::memory::{FrameMemory, PixelFormat},
        pipeline::defaults::DEFAULT_LETTERBOX_PAD_VALUE,
    };
    use fast_image_resize::{images::Image, PixelType, Resizer};
    use ndarray::Array4;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::{domain::prelude::BoundingBox, enums::ai::TensorDType};
    use rayon::prelude::*;
    use std::borrow::Cow;

    // ── Coordinate transform ───────────────────────────────────────

    /// Describes how preprocessed coordinates map back to the original frame.
    ///
    /// Postprocessors use this to reverse any padding/scaling applied during
    /// preprocessing so that bounding boxes align with the original image.
    #[derive(Debug, Clone, Copy)]
    pub struct CoordinateTransform {
        /// Scale factor applied to the original frame (before padding).
        pub scale_x: f32,
        pub scale_y: f32,
        /// Padding offset added after scaling (letterbox only).
        pub pad_x: f32,
        pub pad_y: f32,
        /// Original frame dimensions.
        pub orig_width: u32,
        pub orig_height: u32,
        /// Model input dimensions.
        pub input_width: u32,
        pub input_height: u32,
    }

    impl CoordinateTransform {
        /// Map a bounding box from model output pixel space to normalized
        /// original frame space.
        #[inline]
        pub fn map_bbox_to_original(&self, bbox: &BoundingBox) -> BoundingBox {
            let (x_min, y_min) = self.map_point_to_original(bbox.x_min, bbox.y_min);
            let (x_max, y_max) = self.map_point_to_original(bbox.x_max, bbox.y_max);
            BoundingBox {
                x_min,
                y_min,
                x_max,
                y_max,
            }
        }

        /// Map a single point from model-input normalized space to
        /// original-frame normalized space.
        #[inline]
        pub fn map_point_to_original(&self, x: f32, y: f32) -> (f32, f32) {
            let ox =
                (x * self.input_width as f32 - self.pad_x) / self.scale_x / self.orig_width as f32;
            let oy = (y * self.input_height as f32 - self.pad_y)
                / self.scale_y
                / self.orig_height as f32;
            (ox.clamp(0.0, 1.0), oy.clamp(0.0, 1.0))
        }
    }

    // ── Preprocessing I/O types ────────────────────────────────────

    /// Input context passed to preprocessors.
    pub struct PreprocessInput<'a> {
        /// Decoded video frame (may be CPU or DMA-buf backed).
        pub frame: &'a DecodedFrame,
        /// Model-declared input tensor shape, e.g. `[1, 3, 640, 640]`.
        pub model_input_shape: &'a [i64],
        /// Model-declared input tensor dtype.
        pub model_input_dtype: TensorDType,
    }

    /// Output of preprocessing: either a CPU tensor or a device memory
    /// reference, paired with the coordinate transform for postprocessing.
    ///
    /// Backends dispatch on the variant to select the optimal inference path:
    /// - `CpuTensor` → ONNX Runtime, generic CPU backends
    /// - `DeviceMemory` → RKNN zero-copy (DMA-buf fd), TensorRT (CUDA ptr)
    pub enum PreprocessOutput {
        /// CPU tensor in NCHW float32 layout — for ONNX Runtime and generic
        /// backends that consume `ndarray::Array4<f32>`.
        CpuTensor {
            /// The preprocessed tensor in NCHW format (batch=1).
            tensor: Array4<f32>,
            /// Coordinate transform for mapping postprocessed coordinates back.
            coord_transform: CoordinateTransform,
        },

        /// Device memory reference — for RKNN and TensorRT zero-copy input.
        /// The NPU/GPU can consume this directly without CPU intervention.
        DeviceMemory {
            /// DMA-buf fd or device pointer wrapping the preprocessed frame.
            memory: FrameMemory,
            /// Pixel format of the preprocessed data.
            format: PixelFormat,
            /// Width after resize.
            width: u32,
            /// Height after resize.
            height: u32,
            /// Coordinate transform for mapping postprocessed coordinates back.
            coord_transform: CoordinateTransform,
        },
    }

    impl PreprocessOutput {
        /// Get the coordinate transform regardless of variant.
        #[inline]
        pub fn coord_transform(&self) -> &CoordinateTransform {
            match self {
                Self::CpuTensor {
                    coord_transform, ..
                } => coord_transform,
                Self::DeviceMemory {
                    coord_transform, ..
                } => coord_transform,
            }
        }

        /// Extract the CPU tensor for ONNX-style backends.
        ///
        /// For `CpuTensor`: returns the tensor directly.
        /// For `DeviceMemory`: returns an error — device memory cannot be
        /// converted to `Array4<f32>` without model-specific normalization.
        /// This indicates a backend routing error (ONNX received DMA input).
        pub fn into_cpu_tensor(self) -> Result<Array4<f32>, AiEngineError> {
            match self {
                Self::CpuTensor { tensor, .. } => Ok(tensor),
                Self::DeviceMemory { .. } => Err(AiEngineError::PreprocessError(
                    "cannot convert DeviceMemory PreprocessOutput to CPU tensor; \
                     this indicates a backend routing error"
                        .into(),
                )),
            }
        }
    }

    // ── PreProcessor trait ─────────────────────────────────────────

    /// Pluggable preprocessor — transforms a decoded RGB frame into a
    /// model-ready NCHW tensor.
    ///
    /// Implementations must be `Send + Sync` for concurrent pipeline workers.
    pub trait PreProcessor: Send + Sync + 'static {
        /// Unique identifier for this preprocessor type.
        fn name(&self) -> &str;

        /// Transform a decoded frame into a model-ready input tensor.
        fn process(&self, input: PreprocessInput<'_>) -> Result<PreprocessOutput, AiEngineError>;
    }

    // ── Normalization params ───────────────────────────────────────

    /// Per-channel normalization: `(pixel/255 - mean) / std`.
    #[derive(Debug, Clone, Copy)]
    pub struct NormalizationParams {
        pub mean: [f32; 3],
        pub std: [f32; 3],
    }

    impl NormalizationParams {
        /// YOLO family: `÷255` only (mean=0, std=1).
        pub const YOLO: Self = Self {
            mean: [0.0, 0.0, 0.0],
            std: [1.0, 1.0, 1.0],
        };

        /// ImageNet normalization (ResNet, EfficientNet, MobileNet, etc.).
        pub const IMAGENET: Self = Self {
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
        };

        /// Symmetric `[-1, 1]` normalization (some GAN/ViT models).
        pub const SYMMETRIC: Self = Self {
            mean: [0.5, 0.5, 0.5],
            std: [0.5, 0.5, 0.5],
        };

        /// Resolve a normalization preset name to parameters.
        pub fn from_preset(name: &str) -> Option<Self> {
            match name {
                "yolo" => Some(Self::YOLO),
                "imagenet" => Some(Self::IMAGENET),
                "symmetric" => Some(Self::SYMMETRIC),
                _ => None,
            }
        }
    }

    // ── Built-in: Letterbox ────────────────────────────────────────

    /// Letterbox preprocessor — preserves aspect ratio with padding.
    ///
    /// Standard for YOLO family models:
    /// 1. Scale image so the longer side fits the target
    /// 2. Pad the shorter side with a fill color (default: 114 gray)
    /// 3. Normalize and convert HWC → NCHW
    pub struct LetterboxPreProcessor {
        pub pad_value: u8,
        pub normalize: NormalizationParams,
        pub rgb_order: bool,
    }

    impl Default for LetterboxPreProcessor {
        fn default() -> Self {
            Self {
                pad_value: DEFAULT_LETTERBOX_PAD_VALUE,
                normalize: NormalizationParams::YOLO,
                rgb_order: true,
            }
        }
    }

    impl PreProcessor for LetterboxPreProcessor {
        fn name(&self) -> &str {
            "letterbox"
        }

        fn process(&self, input: PreprocessInput<'_>) -> Result<PreprocessOutput, AiEngineError> {
            let frame = input.frame;
            let target_h = input.model_input_shape[2] as u32;
            let target_w = input.model_input_shape[3] as u32;
            let channels = if is_single_channel(input.model_input_shape) {
                1
            } else {
                3
            };

            let scale = f32::min(
                target_w as f32 / frame.width as f32,
                target_h as f32 / frame.height as f32,
            );
            let new_w = (frame.width as f32 * scale).round() as u32;
            let new_h = (frame.height as f32 * scale).round() as u32;

            let resized = resize_rgb(frame, new_w, new_h)?;

            let pad_x = ((target_w - new_w) as f32 / 2.0).round() as u32;
            let pad_y = ((target_h - new_h) as f32 / 2.0).round() as u32;
            let h = target_h as usize;
            let w = target_w as usize;
            let mut tensor = Array4::<f32>::zeros((1, channels, h, w));
            let tensor_data = tensor.as_slice_mut().ok_or(AiEngineError::PreprocessError(
                "tensor storage must be contiguous".into(),
            ))?;
            let plane_size = h * w;
            fill_tensor_with_normalized_pad(
                tensor_data,
                plane_size,
                channels,
                self.pad_value,
                &self.normalize,
            );

            let pixel_params = NormalizedPixelWrite {
                pixels: &resized,
                source_width: new_w,
                source_offset_x: 0,
                source_offset_y: 0,
                copy_w: new_w,
                copy_h: new_h,
                offset_x: pad_x,
                offset_y: pad_y,
                norm: &self.normalize,
                rgb_order: self.rgb_order,
            };

            if channels == 1 {
                write_grayscale_pixels(tensor_data, w, h, &pixel_params);
            } else {
                write_normalized_pixels(tensor_data, w, h, &pixel_params);
            }

            Ok(PreprocessOutput::CpuTensor {
                tensor,
                coord_transform: CoordinateTransform {
                    scale_x: scale,
                    scale_y: scale,
                    pad_x: pad_x as f32,
                    pad_y: pad_y as f32,
                    orig_width: frame.width,
                    orig_height: frame.height,
                    input_width: target_w,
                    input_height: target_h,
                },
            })
        }
    }

    // ── Built-in: CenterCrop ───────────────────────────────────────

    /// Center-crop preprocessor — standard for classification models.
    ///
    /// 1. Resize the shorter side to target size
    /// 2. Center-crop to target dimensions
    /// 3. Normalize with ImageNet mean/std (default)
    pub struct CenterCropPreProcessor {
        pub normalize: NormalizationParams,
        pub rgb_order: bool,
    }

    impl Default for CenterCropPreProcessor {
        fn default() -> Self {
            Self {
                normalize: NormalizationParams::IMAGENET,
                rgb_order: true,
            }
        }
    }

    impl PreProcessor for CenterCropPreProcessor {
        fn name(&self) -> &str {
            "center_crop"
        }

        fn process(&self, input: PreprocessInput<'_>) -> Result<PreprocessOutput, AiEngineError> {
            let frame = input.frame;
            let target_h = input.model_input_shape[2] as u32;
            let target_w = input.model_input_shape[3] as u32;
            let channels = if is_single_channel(input.model_input_shape) {
                1
            } else {
                3
            };

            // Resize so shorter side matches target
            let scale = f32::max(
                target_w as f32 / frame.width as f32,
                target_h as f32 / frame.height as f32,
            );
            let new_w = (frame.width as f32 * scale).round() as u32;
            let new_h = (frame.height as f32 * scale).round() as u32;

            let resized = resize_rgb(frame, new_w, new_h)?;

            // Center crop
            let crop_x = ((new_w - target_w) / 2) as usize;
            let crop_y = ((new_h - target_h) / 2) as usize;
            let h = target_h as usize;
            let w = target_w as usize;
            let mut tensor = Array4::<f32>::zeros((1, channels, h, w));
            let tensor_data = tensor.as_slice_mut().ok_or(AiEngineError::PreprocessError(
                "tensor storage must be contiguous".into(),
            ))?;

            let pixel_params = NormalizedPixelWrite {
                pixels: &resized,
                source_width: new_w,
                source_offset_x: crop_x as u32,
                source_offset_y: crop_y as u32,
                copy_w: target_w,
                copy_h: target_h,
                offset_x: 0,
                offset_y: 0,
                norm: &self.normalize,
                rgb_order: self.rgb_order,
            };

            if channels == 1 {
                write_grayscale_pixels(tensor_data, w, h, &pixel_params);
            } else {
                write_normalized_pixels(tensor_data, w, h, &pixel_params);
            }

            Ok(PreprocessOutput::CpuTensor {
                tensor,
                coord_transform: CoordinateTransform {
                    scale_x: scale,
                    scale_y: scale,
                    pad_x: 0.0,
                    pad_y: 0.0,
                    orig_width: frame.width,
                    orig_height: frame.height,
                    input_width: target_w,
                    input_height: target_h,
                },
            })
        }
    }

    // ── Built-in: DirectResize ─────────────────────────────────────

    /// Direct resize preprocessor — stretches to target size without
    /// preserving aspect ratio. Used when aspect ratio preservation is
    /// not critical (some segmentation/anomaly models).
    pub struct DirectResizePreProcessor {
        pub normalize: NormalizationParams,
        pub rgb_order: bool,
    }

    impl Default for DirectResizePreProcessor {
        fn default() -> Self {
            Self {
                normalize: NormalizationParams::YOLO,
                rgb_order: true,
            }
        }
    }

    impl PreProcessor for DirectResizePreProcessor {
        fn name(&self) -> &str {
            "direct_resize"
        }

        fn process(&self, input: PreprocessInput<'_>) -> Result<PreprocessOutput, AiEngineError> {
            let frame = input.frame;
            let target_h = input.model_input_shape[2] as u32;
            let target_w = input.model_input_shape[3] as u32;
            let channels = if is_single_channel(input.model_input_shape) {
                1
            } else {
                3
            };

            let resized = resize_rgb(frame, target_w, target_h)?;

            let h = target_h as usize;
            let w = target_w as usize;
            let mut tensor = Array4::<f32>::zeros((1, channels, h, w));
            let tensor_data = tensor.as_slice_mut().ok_or(AiEngineError::PreprocessError(
                "tensor storage must be contiguous".into(),
            ))?;

            let pixel_params = NormalizedPixelWrite {
                pixels: &resized,
                source_width: target_w,
                source_offset_x: 0,
                source_offset_y: 0,
                copy_w: target_w,
                copy_h: target_h,
                offset_x: 0,
                offset_y: 0,
                norm: &self.normalize,
                rgb_order: self.rgb_order,
            };

            if channels == 1 {
                write_grayscale_pixels(tensor_data, w, h, &pixel_params);
            } else {
                write_normalized_pixels(tensor_data, w, h, &pixel_params);
            }

            let scale_x = target_w as f32 / frame.width as f32;
            let scale_y = target_h as f32 / frame.height as f32;

            Ok(PreprocessOutput::CpuTensor {
                tensor,
                coord_transform: CoordinateTransform {
                    scale_x,
                    scale_y,
                    pad_x: 0.0,
                    pad_y: 0.0,
                    orig_width: frame.width,
                    orig_height: frame.height,
                    input_width: target_w,
                    input_height: target_h,
                },
            })
        }
    }

    // ── Built-in: RknnLetterbox (uint8 NHWC) ────────────────────────

    /// RKNN-optimized letterbox preprocessor — outputs uint8 NHWC directly.
    ///
    /// Designed for quantized RKNN models that expect uint8 input. Skips
    /// the float32 normalization and NCHW transpose entirely, producing a
    /// `PreprocessOutput::DeviceMemory` with CPU-backed bytes in NHWC layout.
    ///
    /// When the input frame is already DMA-buf backed (e.g. from GStreamer
    /// RGA resize on RK3588), the DMA-buf is passed through directly
    /// as `DeviceMemory` for zero-copy NPU ingestion.
    pub struct RknnLetterboxPreProcessor {
        pub pad_value: u8,
    }

    impl Default for RknnLetterboxPreProcessor {
        fn default() -> Self {
            Self {
                pad_value: DEFAULT_LETTERBOX_PAD_VALUE,
            }
        }
    }

    impl PreProcessor for RknnLetterboxPreProcessor {
        fn name(&self) -> &str {
            "rknn_letterbox"
        }

        fn process(&self, input: PreprocessInput<'_>) -> Result<PreprocessOutput, AiEngineError> {
            let frame = input.frame;
            // RKNN models declare shape as NHWC: [1, H, W, 3]
            // But some export as NCHW: [1, 3, H, W]. Handle both.
            let (target_h, target_w) = if input.model_input_shape.len() == 4 {
                let d1 = input.model_input_shape[1] as u32;
                let d2 = input.model_input_shape[2] as u32;
                let d3 = input.model_input_shape[3] as u32;
                if d1 <= 4 {
                    // NCHW: [1, C, H, W]
                    (d2, d3)
                } else {
                    // NHWC: [1, H, W, C]
                    (d1, d2)
                }
            } else {
                return Err(AiEngineError::PreprocessError(
                    "RKNN model input shape must be 4-dimensional".into(),
                ));
            };

            // If the frame is DMA-buf and already the right size, pass through
            // the DMA-buf fd directly for zero-copy NPU ingestion.
            // The RKNN backend will use rknn_create_mem_from_fd() to import
            // this fd without any CPU-side data movement.
            #[cfg(feature = "dmabuf")]
            if frame.memory.is_dma_buf()
                && frame.width == target_w
                && frame.height == target_h
                && frame.pixel_format == PixelFormat::Rgb24
            {
                let coord_transform = CoordinateTransform {
                    scale_x: 1.0,
                    scale_y: 1.0,
                    pad_x: 0.0,
                    pad_y: 0.0,
                    orig_width: frame.width,
                    orig_height: frame.height,
                    input_width: target_w,
                    input_height: target_h,
                };
                return Ok(PreprocessOutput::DeviceMemory {
                    memory: frame.memory.try_clone()?,
                    format: PixelFormat::Rgb24,
                    width: target_w,
                    height: target_h,
                    coord_transform,
                });
            }

            // CPU path: letterbox resize to uint8 NHWC.
            let scale = f32::min(
                target_w as f32 / frame.width as f32,
                target_h as f32 / frame.height as f32,
            );
            let new_w = (frame.width as f32 * scale).round() as u32;
            let new_h = (frame.height as f32 * scale).round() as u32;

            let resized = resize_rgb(frame, new_w, new_h)?;

            let pad_x = ((target_w - new_w) as f32 / 2.0).round() as u32;
            let pad_y = ((target_h - new_h) as f32 / 2.0).round() as u32;

            // Output: HWC uint8 buffer with letterbox padding.
            let total_bytes = (target_h * target_w * 3) as usize;
            let mut output = vec![self.pad_value; total_bytes];

            let dst_stride = (target_w * 3) as usize;
            let src_stride = (new_w * 3) as usize;

            for y in 0..new_h as usize {
                let dst_row_start = ((pad_y as usize + y) * dst_stride) + (pad_x as usize * 3);
                let src_row_start = y * src_stride;
                output[dst_row_start..dst_row_start + src_stride]
                    .copy_from_slice(&resized[src_row_start..src_row_start + src_stride]);
            }

            let coord_transform = CoordinateTransform {
                scale_x: scale,
                scale_y: scale,
                pad_x: pad_x as f32,
                pad_y: pad_y as f32,
                orig_width: frame.width,
                orig_height: frame.height,
                input_width: target_w,
                input_height: target_h,
            };

            Ok(PreprocessOutput::DeviceMemory {
                memory: FrameMemory::Cpu(bytes::Bytes::from(output)),
                format: PixelFormat::Rgb24,
                width: target_w,
                height: target_h,
                coord_transform,
            })
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────

    /// SIMD-accelerated image resize using `fast_image_resize`.
    ///
    /// Returns `Cow::Borrowed` when no resize is needed and the frame is CPU-resident.
    /// Handles DMA-buf frames by materializing to CPU first via `to_cpu()`.
    ///
    /// # Hardware resize gap
    ///
    /// When the frame is DMA-buf backed, resize still goes through CPU
    /// (`to_cpu()` + `fast_image_resize`). True hardware resize would need:
    /// - **Rockchip**: `rkrgafilter` element in the GStreamer decode pipeline
    ///   (already present for CSC — extend to handle resize in-pipeline)
    /// - **Jetson**: `nvvidconv` with target caps set to the model input size
    /// - **VA-API**: `vaapipostproc` with scale properties
    ///
    /// These are pipeline-level integrations and cannot be done inside the
    /// preprocessor. When the GStreamer pipeline is configured to output the
    /// model's expected resolution, the `RknnLetterboxPreProcessor` DMA-buf
    /// pass-through path above avoids this resize entirely.
    fn resize_rgb<'a>(
        frame: &'a DecodedFrame,
        new_w: u32,
        new_h: u32,
    ) -> Result<Cow<'a, [u8]>, AiEngineError> {
        // Fast path: no resize needed and frame is CPU-resident.
        if new_w == frame.width && new_h == frame.height {
            if let Some(data) = frame.memory.as_cpu_slice() {
                return Ok(Cow::Borrowed(data));
            }
            // DMA/device frame at native resolution: materialize to CPU.
            let cpu_bytes = frame.memory.to_cpu()?;
            return Ok(Cow::Owned(cpu_bytes.to_vec()));
        }

        // Need resize: always work in CPU space.
        let mut src_buf = if let Some(data) = frame.memory.as_cpu_slice() {
            data.to_vec()
        } else {
            frame.memory.to_cpu()?.to_vec()
        };

        let src = Image::from_slice_u8(frame.width, frame.height, &mut src_buf, PixelType::U8x3)
            .map_err(|e| AiEngineError::PreprocessError(format!("source image error: {e}")))?;

        let mut dst = Image::new(new_w, new_h, PixelType::U8x3);
        let mut resizer = Resizer::new();
        resizer
            .resize(&src, &mut dst, None)
            .map_err(|e| AiEngineError::PreprocessError(format!("resize error: {e}")))?;

        Ok(Cow::Owned(dst.into_vec()))
    }

    /// Parameters for writing resized pixels into an NCHW tensor.
    struct NormalizedPixelWrite<'a> {
        pixels: &'a [u8],
        source_width: u32,
        source_offset_x: u32,
        source_offset_y: u32,
        copy_w: u32,
        copy_h: u32,
        offset_x: u32,
        offset_y: u32,
        norm: &'a NormalizationParams,
        rgb_order: bool,
    }

    /// Fill output tensor with normalized pad value before writing the resized ROI.
    fn fill_tensor_with_normalized_pad(
        tensor_data: &mut [f32],
        plane_size: usize,
        channels: usize,
        pad_value: u8,
        norm: &NormalizationParams,
    ) {
        let pad_norm = pad_value as f32 / 255.0;
        if channels == 1 {
            let fill = (pad_norm - norm.mean[0]) / norm.std[0];
            tensor_data.fill(fill);
            return;
        }
        for c in 0..channels {
            let fill = (pad_norm - norm.mean[c]) / norm.std[c];
            let start = c * plane_size;
            let end = start + plane_size;
            tensor_data[start..end].fill(fill);
        }
    }

    /// Build per-channel lookup tables to map `u8` to normalized `f32`.
    fn build_rgb_normalize_lut(norm: &NormalizationParams) -> [[f32; 256]; 3] {
        let mut lut = [[0.0f32; 256]; 3];
        for (channel, table) in lut.iter_mut().enumerate() {
            let inv_std = 1.0 / norm.std[channel];
            for (value, mapped) in table.iter_mut().enumerate() {
                let pixel = value as f32 * (1.0 / 255.0);
                *mapped = (pixel - norm.mean[channel]) * inv_std;
            }
        }
        lut
    }

    /// Build BT.601 grayscale LUT split by source channel contribution.
    fn build_gray_lut(norm: &NormalizationParams) -> ([f32; 256], [f32; 256], [f32; 256]) {
        let mut r_lut = [0.0f32; 256];
        let mut g_lut = [0.0f32; 256];
        let mut b_lut = [0.0f32; 256];
        let inv_std = 1.0 / norm.std[0];
        let mean = norm.mean[0];
        for value in 0..256usize {
            let vf = value as f32 * (1.0 / 255.0);
            r_lut[value] = (0.299 * vf - mean) * inv_std;
            g_lut[value] = 0.587 * vf * inv_std;
            b_lut[value] = 0.114 * vf * inv_std;
        }
        (r_lut, g_lut, b_lut)
    }

    #[inline]
    fn should_parallelize(copy_w: usize, copy_h: usize) -> bool {
        copy_w.saturating_mul(copy_h) >= 128 * 128
    }

    /// Write resized pixels into an NCHW tensor at the given offset with normalization.
    fn write_normalized_pixels(
        tensor_data: &mut [f32],
        tensor_w: usize,
        tensor_h: usize,
        params: &NormalizedPixelWrite<'_>,
    ) {
        let copy_w = params.copy_w as usize;
        let copy_h = params.copy_h as usize;
        let src_w = params.source_width as usize;
        let src_x = params.source_offset_x as usize;
        let src_y = params.source_offset_y as usize;
        let dst_x = params.offset_x as usize;
        let dst_y = params.offset_y as usize;
        let plane_size = tensor_w * tensor_h;
        let lut = build_rgb_normalize_lut(params.norm);
        let (plane0, rest) = tensor_data.split_at_mut(plane_size);
        let (plane1, plane2) = rest.split_at_mut(plane_size);
        let row_start = dst_y * tensor_w;
        let row_end = row_start + copy_h * tensor_w;
        let rows0 = &mut plane0[row_start..row_end];
        let rows1 = &mut plane1[row_start..row_end];
        let rows2 = &mut plane2[row_start..row_end];

        if should_parallelize(copy_w, copy_h) {
            rows0
                .par_chunks_mut(tensor_w)
                .zip(rows1.par_chunks_mut(tensor_w))
                .zip(rows2.par_chunks_mut(tensor_w))
                .enumerate()
                .for_each(|(y, ((row0, row1), row2))| {
                    let src_row_base = (src_y + y) * src_w;
                    let out0 = &mut row0[dst_x..dst_x + copy_w];
                    let out1 = &mut row1[dst_x..dst_x + copy_w];
                    let out2 = &mut row2[dst_x..dst_x + copy_w];
                    for x in 0..copy_w {
                        let src_idx = (src_row_base + src_x + x) * 3;
                        let (c0, c1, c2) = if params.rgb_order {
                            (
                                params.pixels[src_idx] as usize,
                                params.pixels[src_idx + 1] as usize,
                                params.pixels[src_idx + 2] as usize,
                            )
                        } else {
                            (
                                params.pixels[src_idx + 2] as usize,
                                params.pixels[src_idx + 1] as usize,
                                params.pixels[src_idx] as usize,
                            )
                        };
                        out0[x] = lut[0][c0];
                        out1[x] = lut[1][c1];
                        out2[x] = lut[2][c2];
                    }
                });
            return;
        }

        for (y, ((row0, row1), row2)) in rows0
            .chunks_mut(tensor_w)
            .zip(rows1.chunks_mut(tensor_w))
            .zip(rows2.chunks_mut(tensor_w))
            .enumerate()
        {
            let src_row_base = (src_y + y) * src_w;
            let out0 = &mut row0[dst_x..dst_x + copy_w];
            let out1 = &mut row1[dst_x..dst_x + copy_w];
            let out2 = &mut row2[dst_x..dst_x + copy_w];
            for x in 0..copy_w {
                let src_idx = (src_row_base + src_x + x) * 3;
                let (c0, c1, c2) = if params.rgb_order {
                    (
                        params.pixels[src_idx] as usize,
                        params.pixels[src_idx + 1] as usize,
                        params.pixels[src_idx + 2] as usize,
                    )
                } else {
                    (
                        params.pixels[src_idx + 2] as usize,
                        params.pixels[src_idx + 1] as usize,
                        params.pixels[src_idx] as usize,
                    )
                };
                out0[x] = lut[0][c0];
                out1[x] = lut[1][c1];
                out2[x] = lut[2][c2];
            }
        }
    }

    /// Write resized RGB pixels as grayscale into a single-channel NCHW tensor.
    ///
    /// Converts RGB to luminance using BT.601 coefficients:
    /// `L = 0.299R + 0.587G + 0.114B`
    fn write_grayscale_pixels(
        tensor_data: &mut [f32],
        tensor_w: usize,
        _tensor_h: usize,
        params: &NormalizedPixelWrite<'_>,
    ) {
        let copy_w = params.copy_w as usize;
        let copy_h = params.copy_h as usize;
        let src_w = params.source_width as usize;
        let src_x = params.source_offset_x as usize;
        let src_y = params.source_offset_y as usize;
        let dst_x = params.offset_x as usize;
        let dst_y = params.offset_y as usize;
        let (r_lut, g_lut, b_lut) = build_gray_lut(params.norm);
        let row_start = dst_y * tensor_w;
        let row_end = row_start + copy_h * tensor_w;
        let rows = &mut tensor_data[row_start..row_end];

        if should_parallelize(copy_w, copy_h) {
            rows.par_chunks_mut(tensor_w)
                .enumerate()
                .for_each(|(y, row)| {
                    let src_row_base = (src_y + y) * src_w;
                    let out = &mut row[dst_x..dst_x + copy_w];
                    for (x, out_pixel) in out.iter_mut().enumerate() {
                        let src_idx = (src_row_base + src_x + x) * 3;
                        let r = params.pixels[src_idx] as usize;
                        let g = params.pixels[src_idx + 1] as usize;
                        let b = params.pixels[src_idx + 2] as usize;
                        *out_pixel = r_lut[r] + g_lut[g] + b_lut[b];
                    }
                });
            return;
        }

        for (y, row) in rows.chunks_mut(tensor_w).enumerate() {
            let src_row_base = (src_y + y) * src_w;
            let out = &mut row[dst_x..dst_x + copy_w];
            for (x, out_pixel) in out.iter_mut().enumerate() {
                let src_idx = (src_row_base + src_x + x) * 3;
                let r = params.pixels[src_idx] as usize;
                let g = params.pixels[src_idx + 1] as usize;
                let b = params.pixels[src_idx + 2] as usize;
                *out_pixel = r_lut[r] + g_lut[g] + b_lut[b];
            }
        }
    }

    /// Check if the model expects single-channel (grayscale) input.
    #[inline]
    fn is_single_channel(model_input_shape: &[i64]) -> bool {
        model_input_shape.len() == 4 && model_input_shape[1] == 1
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::test_utils::*;
        use approx::assert_abs_diff_eq;
        use ng_gateway_models::enums::ai::TensorDType;
        use proptest::prelude::*;

        /// Standard NCHW shape for YOLO-family models.
        const SHAPE_3_640: [i64; 4] = [1, 3, 640, 640];
        /// Classification model input shape (e.g. ResNet).
        const SHAPE_3_224: [i64; 4] = [1, 3, 224, 224];
        /// Single-channel (grayscale) input shape.
        const SHAPE_1_640: [i64; 4] = [1, 1, 640, 640];

        // ── 1. Letterbox: 1080p → 640×640 preserves aspect and pads ─────

        #[test]
        fn letterbox_1080p_to_640_preserves_aspect_and_pads() {
            let frame = make_solid_frame(1920, 1080, 128, 128, 128);
            let pp = LetterboxPreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_640,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("letterbox should succeed");

            let PreprocessOutput::CpuTensor {
                ref tensor,
                ref coord_transform,
            } = output
            else {
                panic!("expected CpuTensor variant");
            };

            assert_eq!(tensor.shape(), &[1, 3, 640, 640]);

            // 1920×1080 → scale = 640/1920 = 0.3333
            // new_h = round(1080 * 0.3333) = 360 → pad_y = (640-360)/2 = 140
            assert!(coord_transform.pad_y > 0.0, "vertical padding expected");
            assert_abs_diff_eq!(coord_transform.pad_x, 0.0, epsilon = 1.0);

            // Verify pad region holds normalized pad_value (114/255 ≈ 0.4471
            // with YOLO norm mean=0 std=1).
            let expected_pad = 114.0 / 255.0;
            let pad_top_rows = coord_transform.pad_y as usize;
            for c in 0..3 {
                for y in 0..pad_top_rows {
                    for x in 0..640 {
                        let val = tensor[[0, c, y, x]];
                        assert_abs_diff_eq!(val, expected_pad, epsilon = 0.01);
                    }
                }
            }
        }

        // ── 2. Letterbox: square input → no padding ─────────────────────

        #[test]
        fn letterbox_square_input_no_pad() {
            let frame = make_solid_frame(640, 640, 64, 64, 64);
            let pp = LetterboxPreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_640,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("letterbox should succeed");

            let ct = output.coord_transform();
            assert_abs_diff_eq!(ct.pad_x, 0.0, epsilon = 1e-6);
            assert_abs_diff_eq!(ct.pad_y, 0.0, epsilon = 1e-6);
        }

        // ── 3. DirectResize: output matches target shape ────────────────

        #[test]
        fn direct_resize_output_matches_target() {
            let frame = make_gradient_frame(1920, 1080);
            let pp = DirectResizePreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_640,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("direct resize should succeed");

            let PreprocessOutput::CpuTensor { ref tensor, .. } = output else {
                panic!("expected CpuTensor variant");
            };
            assert_eq!(tensor.shape(), &[1, 3, 640, 640]);
        }

        // ── 4. CenterCrop: output dimensions ────────────────────────────

        #[test]
        fn center_crop_output_dimensions() {
            let frame = make_gradient_frame(1920, 1080);
            let pp = CenterCropPreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_224,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("center crop should succeed");

            let PreprocessOutput::CpuTensor { ref tensor, .. } = output else {
                panic!("expected CpuTensor variant");
            };
            assert_eq!(tensor.shape(), &[1, 3, 224, 224]);
        }

        // ── 5. Coordinate transform roundtrip for letterbox ─────────────

        #[test]
        fn coordinate_transform_map_bbox_letterbox_roundtrip() {
            let frame = make_solid_frame(1920, 1080, 100, 100, 100);
            let pp = LetterboxPreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_640,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("letterbox should succeed");

            let ct = output.coord_transform();

            // A normalized bbox roughly centered in model-input space.
            let model_bbox = BoundingBox {
                x_min: 0.25,
                y_min: 0.25,
                x_max: 0.75,
                y_max: 0.75,
            };
            let orig = ct.map_bbox_to_original(&model_bbox);

            // Mapped bbox must be within [0, 1] original-frame range.
            assert!(orig.x_min >= 0.0 && orig.x_min <= 1.0);
            assert!(orig.y_min >= 0.0 && orig.y_min <= 1.0);
            assert!(orig.x_max >= 0.0 && orig.x_max <= 1.0);
            assert!(orig.y_max >= 0.0 && orig.y_max <= 1.0);
            // Box should maintain ordering.
            assert!(orig.x_max > orig.x_min);
            assert!(orig.y_max > orig.y_min);
        }

        // ── 6. Coordinate transform identity when sizes match ───────────

        #[test]
        fn coordinate_transform_identity_when_sizes_match() {
            let frame = make_solid_frame(640, 640, 50, 50, 50);
            let pp = DirectResizePreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_640,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("direct resize should succeed");

            let ct = output.coord_transform();
            assert_abs_diff_eq!(ct.scale_x, 1.0, epsilon = 1e-6);
            assert_abs_diff_eq!(ct.scale_y, 1.0, epsilon = 1e-6);
            assert_abs_diff_eq!(ct.pad_x, 0.0, epsilon = 1e-6);
            assert_abs_diff_eq!(ct.pad_y, 0.0, epsilon = 1e-6);
        }

        // ── 7. YOLO normalization: pixel value mapping ──────────────────

        #[test]
        fn normalization_yolo_pixel_values() {
            // Solid white (255,255,255) → should normalize to 1.0 with YOLO.
            let white_frame = make_solid_frame(32, 32, 255, 255, 255);
            let pp = DirectResizePreProcessor {
                normalize: NormalizationParams::YOLO,
                rgb_order: true,
            };
            let output = pp
                .process(PreprocessInput {
                    frame: &white_frame,
                    model_input_shape: &[1, 3, 32, 32],
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("preprocess should succeed");

            let PreprocessOutput::CpuTensor { ref tensor, .. } = output else {
                panic!("expected CpuTensor variant");
            };
            assert_abs_diff_eq!(tensor[[0, 0, 0, 0]], 1.0, epsilon = 1e-4);

            // Solid black (0,0,0) → should normalize to 0.0 with YOLO.
            let black_frame = make_solid_frame(32, 32, 0, 0, 0);
            let output = pp
                .process(PreprocessInput {
                    frame: &black_frame,
                    model_input_shape: &[1, 3, 32, 32],
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("preprocess should succeed");

            let PreprocessOutput::CpuTensor { ref tensor, .. } = output else {
                panic!("expected CpuTensor variant");
            };
            assert_abs_diff_eq!(tensor[[0, 0, 0, 0]], 0.0, epsilon = 1e-6);
        }

        // ── 8. ImageNet normalization: pixel value mapping ──────────────

        #[test]
        fn normalization_imagenet_pixel_values() {
            // pixel=128 → channel R: (128/255 - 0.485) / 0.229 ≈ 0.0501
            let frame = make_solid_frame(32, 32, 128, 128, 128);
            let pp = DirectResizePreProcessor {
                normalize: NormalizationParams::IMAGENET,
                rgb_order: true,
            };
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &[1, 3, 32, 32],
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("preprocess should succeed");

            let PreprocessOutput::CpuTensor { ref tensor, .. } = output else {
                panic!("expected CpuTensor variant");
            };

            let expected_r = (128.0_f32 / 255.0 - 0.485) / 0.229;
            let expected_g = (128.0_f32 / 255.0 - 0.456) / 0.224;
            let expected_b = (128.0_f32 / 255.0 - 0.406) / 0.225;

            assert_abs_diff_eq!(tensor[[0, 0, 0, 0]], expected_r, epsilon = 0.01);
            assert_abs_diff_eq!(tensor[[0, 1, 0, 0]], expected_g, epsilon = 0.01);
            assert_abs_diff_eq!(tensor[[0, 2, 0, 0]], expected_b, epsilon = 0.01);
        }

        // ── 9. RKNN letterbox: outputs NHWC uint8 DeviceMemory ──────────

        #[test]
        fn rknn_letterbox_outputs_nhwc_uint8() {
            let frame = make_gradient_frame(1920, 1080);
            let pp = RknnLetterboxPreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_640,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("rknn letterbox should succeed");

            let PreprocessOutput::DeviceMemory {
                ref memory,
                format,
                width,
                height,
                ..
            } = output
            else {
                panic!("expected DeviceMemory variant");
            };

            assert_eq!(format, PixelFormat::Rgb24);
            assert_eq!(width, 640);
            assert_eq!(height, 640);

            // Verify CPU bytes length matches HWC layout: H × W × 3.
            let cpu_bytes = memory
                .as_cpu_slice()
                .expect("RKNN letterbox CPU path should produce Cpu variant");
            assert_eq!(cpu_bytes.len(), 640 * 640 * 3);
        }

        // ── 10. Single-channel (grayscale) preprocessing ────────────────

        #[test]
        fn preprocess_single_channel_gray() {
            let frame = make_solid_frame(320, 240, 100, 150, 200);
            let pp = LetterboxPreProcessor::default();
            let output = pp
                .process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_1_640,
                    model_input_dtype: TensorDType::Float32,
                })
                .expect("single-channel preprocess should succeed");

            let PreprocessOutput::CpuTensor { ref tensor, .. } = output else {
                panic!("expected CpuTensor variant");
            };
            assert_eq!(tensor.shape(), &[1, 1, 640, 640]);
        }

        // ── 11. Zero-dimension frame → error ────────────────────────────

        #[test]
        fn preprocess_zero_dimension_frame_returns_error() {
            let frame = make_solid_frame(0, 0, 0, 0, 0);
            let pp = LetterboxPreProcessor::default();
            let result = pp.process(PreprocessInput {
                frame: &frame,
                model_input_shape: &SHAPE_3_640,
                model_input_dtype: TensorDType::Float32,
            });
            // A 0×0 frame should either error or produce a degenerate output.
            // We accept both: an explicit error is ideal, but a tensor filled
            // entirely with pad values is also correct behavior.
            match result {
                Err(_) => {} // expected
                Ok(PreprocessOutput::CpuTensor { ref tensor, .. }) => {
                    assert_eq!(tensor.shape(), &[1, 3, 640, 640]);
                    // All values should be the normalized pad fill.
                    let expected_pad = 114.0 / 255.0;
                    for &v in tensor.iter() {
                        assert_abs_diff_eq!(v, expected_pad, epsilon = 0.01);
                    }
                }
                Ok(_) => panic!("unexpected DeviceMemory from CPU letterbox"),
            }
        }

        // ── 12. Property test: letterbox output within sane range ───────

        proptest! {
            #[test]
            fn proptest_letterbox_output_within_unit_range(
                w in 1u32..2048,
                h in 1u32..2048,
            ) {
                let frame = make_solid_frame(w, h, 128, 128, 128);
                let pp = LetterboxPreProcessor::default();
                let result = pp.process(PreprocessInput {
                    frame: &frame,
                    model_input_shape: &SHAPE_3_640,
                    model_input_dtype: TensorDType::Float32,
                });
                if let Ok(PreprocessOutput::CpuTensor { ref tensor, .. }) = result {
                    for &val in tensor.iter() {
                        prop_assert!(
                            (-3.0..=3.0).contains(&val),
                            "tensor value {} outside [-3.0, 3.0]",
                            val
                        );
                    }
                }
            }
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
