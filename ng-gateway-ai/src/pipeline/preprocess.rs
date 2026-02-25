//! Preprocessing trait and built-in implementations.
//!
//! Engine-internal module: NOT exposed to users or drivers.
//! Users control preprocessing via `PreProcessorConfig` parameters in
//! the Pipeline API; the engine maps those to the correct implementation.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded_frame::DecodedFrame;
    use ndarray::Array4;
    use ng_gateway_models::ai::{model::TensorDType, types::BoundingBox};
    use ng_gateway_error::ai::AiEngineError;

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
                pad_value: 114,
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

            // Fill padding
            let pad_norm = self.pad_value as f32 / 255.0;
            for c in 0..channels {
                let fill = (pad_norm - self.normalize.mean[c]) / self.normalize.std[c];
                tensor.slice_mut(ndarray::s![0, c, .., ..]).fill(fill);
            }

            let pixel_params = NormalizedPixelWrite {
                pixels: &resized,
                w: new_w,
                h: new_h,
                offset_x: pad_x,
                offset_y: pad_y,
                norm: &self.normalize,
                rgb_order: self.rgb_order,
            };

            if channels == 1 {
                write_grayscale_pixels(&mut tensor, &pixel_params);
            } else {
                write_normalized_pixels(&mut tensor, &pixel_params);
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

            if channels == 1 {
                let mean = self.normalize.mean[0];
                let std = self.normalize.std[0];
                for y in 0..h {
                    for x in 0..w {
                        let src_idx = ((crop_y + y) * new_w as usize + (crop_x + x)) * 3;
                        let r = resized[src_idx] as f32;
                        let g = resized[src_idx + 1] as f32;
                        let b = resized[src_idx + 2] as f32;
                        let gray = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
                        tensor[[0, 0, y, x]] = (gray - mean) / std;
                    }
                }
            } else {
                for y in 0..h {
                    for x in 0..w {
                        let src_idx = ((crop_y + y) * new_w as usize + (crop_x + x)) * 3;
                        for c in 0..3usize {
                            let ch = if self.rgb_order { c } else { 2 - c };
                            let pixel = resized[src_idx + ch] as f32 / 255.0;
                            tensor[[0, c, y, x]] =
                                (pixel - self.normalize.mean[c]) / self.normalize.std[c];
                        }
                    }
                }
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

            let pixel_params = NormalizedPixelWrite {
                pixels: &resized,
                w: target_w,
                h: target_h,
                offset_x: 0,
                offset_y: 0,
                norm: &self.normalize,
                rgb_order: self.rgb_order,
            };

            if channels == 1 {
                write_grayscale_pixels(&mut tensor, &pixel_params);
            } else {
                write_normalized_pixels(&mut tensor, &pixel_params);
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
    fn resize_rgb(frame: &DecodedFrame, new_w: u32, new_h: u32) -> Result<Vec<u8>, AiEngineError> {
        use fast_image_resize::images::Image;
        use fast_image_resize::{PixelType, Resizer};

        if new_w == frame.width && new_h == frame.height {
            return Ok(frame.data.clone());
        }

        let src = Image::from_vec_u8(
            frame.width,
            frame.height,
            frame.data.clone(),
            PixelType::U8x3,
        )
        .map_err(|e| AiEngineError::PreprocessError(format!("source image error: {e}")))?;

        let mut dst = Image::new(new_w, new_h, PixelType::U8x3);
        let mut resizer = Resizer::new();
        resizer
            .resize(&src, &mut dst, None)
            .map_err(|e| AiEngineError::PreprocessError(format!("resize error: {e}")))?;

        Ok(dst.into_vec())
    }

    /// Parameters for writing resized pixels into an NCHW tensor.
    struct NormalizedPixelWrite<'a> {
        pixels: &'a [u8],
        w: u32,
        h: u32,
        offset_x: u32,
        offset_y: u32,
        norm: &'a NormalizationParams,
        rgb_order: bool,
    }

    /// Write resized pixels into an NCHW tensor at the given offset with normalization.
    fn write_normalized_pixels(tensor: &mut Array4<f32>, params: &NormalizedPixelWrite<'_>) {
        let w = params.w as usize;
        let h = params.h as usize;
        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 3;
                let dst_y = y + params.offset_y as usize;
                let dst_x = x + params.offset_x as usize;
                for c in 0..3usize {
                    let ch = if params.rgb_order { c } else { 2 - c };
                    let pixel = params.pixels[src_idx + ch] as f32 / 255.0;
                    tensor[[0, c, dst_y, dst_x]] =
                        (pixel - params.norm.mean[c]) / params.norm.std[c];
                }
            }
        }
    }

    /// Write resized RGB pixels as grayscale into a single-channel NCHW tensor.
    ///
    /// Converts RGB to luminance using BT.601 coefficients:
    /// `L = 0.299R + 0.587G + 0.114B`
    fn write_grayscale_pixels(
        tensor: &mut ndarray::Array4<f32>,
        params: &NormalizedPixelWrite<'_>,
    ) {
        let w = params.w as usize;
        let h = params.h as usize;
        let mean = params.norm.mean[0];
        let std = params.norm.std[0];
        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 3;
                let r = params.pixels[src_idx] as f32;
                let g = params.pixels[src_idx + 1] as f32;
                let b = params.pixels[src_idx + 2] as f32;
                let gray = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
                let dst_y = y + params.offset_y as usize;
                let dst_x = x + params.offset_x as usize;
                tensor[[0, 0, dst_y, dst_x]] = (gray - mean) / std;
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
