//! Preprocessing trait and built-in implementations.
//!
//! Engine-internal module: NOT exposed to users or drivers.
//! Users control preprocessing via `PreProcessorConfig` parameters in
//! the Pipeline API; the engine maps those to the correct implementation.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded::DecodedFrame;
    use crate::pipeline::defaults::DEFAULT_LETTERBOX_PAD_VALUE;
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
        /// Decoded video frame (RGB24).
        pub frame: &'a DecodedFrame,
        /// Model-declared input tensor shape, e.g. `[1, 3, 640, 640]`.
        pub model_input_shape: &'a [i64],
        /// Model-declared input tensor dtype.
        pub model_input_dtype: TensorDType,
    }

    /// Output of preprocessing: a ready-to-infer tensor + coordinate mapping.
    pub struct PreprocessOutput {
        /// The preprocessed tensor in NCHW format (batch=1).
        pub tensor: Array4<f32>,
        /// Coordinate transform for mapping postprocessed coordinates back.
        pub coord_transform: CoordinateTransform,
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

            Ok(PreprocessOutput {
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

            Ok(PreprocessOutput {
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

            Ok(PreprocessOutput {
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

    // ── Helpers ─────────────────────────────────────────────────────

    /// SIMD-accelerated image resize using `fast_image_resize`.
    ///
    /// Returns `Cow::Borrowed` when no resize is needed (zero-copy fast path).
    /// When resizing, uses `from_slice_u8` on a mutable copy to avoid an
    /// extra allocation inside the Image constructor.
    fn resize_rgb<'a>(
        frame: &'a DecodedFrame,
        new_w: u32,
        new_h: u32,
    ) -> Result<Cow<'a, [u8]>, AiEngineError> {
        if new_w == frame.width && new_h == frame.height {
            return Ok(Cow::Borrowed(frame.data.as_ref()));
        }

        let mut src_buf = frame.data.as_ref().to_vec();
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
}

#[cfg(feature = "engine")]
pub use inner::*;
