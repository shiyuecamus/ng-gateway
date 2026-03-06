//! Shared test utilities for ng-gateway-ai unit and integration tests.
//!
//! Provides builders for synthetic frames, detections, inference outputs,
//! and model metadata — keeping individual test modules focused on
//! assertions rather than boilerplate construction.

#![allow(dead_code)]

use crate::decoded::DecodedFrame;
use bytes::Bytes;
use std::sync::Arc;

// ── Frame helpers ─────────────────────────────────────────────────

/// Create a solid-color RGB24 `DecodedFrame`.
pub fn make_solid_frame(width: u32, height: u32, r: u8, g: u8, b: u8) -> DecodedFrame {
    let pixel_count = width as usize * height as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for _ in 0..pixel_count {
        data.push(r);
        data.push(g);
        data.push(b);
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

/// Create a gradient RGB24 `DecodedFrame` (useful for verifying pixel
/// operations produce correct spatial results).
pub fn make_gradient_frame(width: u32, height: u32) -> DecodedFrame {
    let pixel_count = width as usize * height as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for y in 0..height {
        for x in 0..width {
            data.push(((x * 255) / width.max(1)) as u8);
            data.push(((y * 255) / height.max(1)) as u8);
            data.push(128u8);
        }
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

// ── Detection helpers ─────────────────────────────────────────────

#[cfg(feature = "engine")]
pub use engine_utils::*;

#[cfg(feature = "engine")]
mod engine_utils {
    use super::*;
    use crate::pipeline::postprocess::RawInferenceOutput;
    use crate::pipeline::preprocess::CoordinateTransform;
    use ndarray::{Array3, ArrayD};
    use ng_gateway_models::{
        domain::prelude::{BoundingBox, Detection},
        entities::ai::model::{Labels, TensorDesc, TensorDescs},
        enums::ai::{ModelFormat, ModelTask, TensorDType},
    };

    /// Shorthand detection builder.
    pub fn make_detection(
        class: &str,
        class_id: u32,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        confidence: f32,
    ) -> Detection {
        Detection {
            bbox: BoundingBox {
                x_min: x1,
                y_min: y1,
                x_max: x2,
                y_max: y2,
            },
            class: Arc::from(class),
            class_id,
            confidence,
            track_id: None,
        }
    }

    /// Assign a track ID to an existing detection.
    pub fn with_track_id(mut det: Detection, track_id: u64) -> Detection {
        det.track_id = Some(track_id);
        det
    }

    /// Build a YOLOv8-format raw tensor `[1, 4+C, N]` from explicit detections.
    ///
    /// Each detection is (cx_px, cy_px, w_px, h_px, class_scores...).
    pub fn make_yolov8_output(
        detections: &[(f32, f32, f32, f32, Vec<f32>)],
        num_classes: usize,
    ) -> RawInferenceOutput {
        let num_preds = detections.len();
        let num_features = 4 + num_classes;
        let mut tensor = Array3::<f32>::zeros((1, num_features, num_preds));
        for (i, (cx, cy, w, h, scores)) in detections.iter().enumerate() {
            tensor[[0, 0, i]] = *cx;
            tensor[[0, 1, i]] = *cy;
            tensor[[0, 2, i]] = *w;
            tensor[[0, 3, i]] = *h;
            for (c, &score) in scores.iter().enumerate().take(num_classes) {
                tensor[[0, 4 + c, i]] = score;
            }
        }
        RawInferenceOutput {
            tensors: vec![("output0".to_string(), tensor.into_dyn())],
        }
    }

    /// A single YOLOv5 detection: `(cx, cy, w, h, objectness, class_scores)`.
    pub type Yolov5Detection = (f32, f32, f32, f32, f32, Vec<f32>);

    /// Build a YOLOv5-format raw tensor `[1, N, 5+C]` from explicit detections.
    ///
    /// Each detection is `(cx_px, cy_px, w_px, h_px, objectness, class_scores...)`.
    pub fn make_yolov5_output(
        detections: &[Yolov5Detection],
        num_classes: usize,
    ) -> RawInferenceOutput {
        let num_preds = detections.len();
        let feat_size = 5 + num_classes;
        let mut tensor = Array3::<f32>::zeros((1, num_preds, feat_size));
        for (i, (cx, cy, w, h, obj, scores)) in detections.iter().enumerate() {
            tensor[[0, i, 0]] = *cx;
            tensor[[0, i, 1]] = *cy;
            tensor[[0, i, 2]] = *w;
            tensor[[0, i, 3]] = *h;
            tensor[[0, i, 4]] = *obj;
            for (c, &score) in scores.iter().enumerate().take(num_classes) {
                tensor[[0, i, 5 + c]] = score;
            }
        }
        RawInferenceOutput {
            tensors: vec![("output0".to_string(), tensor.into_dyn())],
        }
    }

    /// Build a classification raw tensor `[1, C]`.
    pub fn make_classification_output(scores: &[f32]) -> RawInferenceOutput {
        let num_classes = scores.len();
        let tensor = ArrayD::from_shape_vec(vec![1, num_classes], scores.to_vec())
            .expect("shape mismatch in make_classification_output");
        RawInferenceOutput {
            tensors: vec![("output0".to_string(), tensor)],
        }
    }

    /// Build a segmentation raw tensor `[1, C, H, W]`.
    pub fn make_segmentation_output(
        data: Vec<f32>,
        c: usize,
        h: usize,
        w: usize,
    ) -> RawInferenceOutput {
        let tensor = ArrayD::from_shape_vec(vec![1, c, h, w], data)
            .expect("shape mismatch in make_segmentation_output");
        RawInferenceOutput {
            tensors: vec![("output0".to_string(), tensor)],
        }
    }

    /// Build an identity `CoordinateTransform` (no scaling, no padding).
    pub fn identity_transform(width: u32, height: u32) -> CoordinateTransform {
        CoordinateTransform {
            scale_x: 1.0,
            scale_y: 1.0,
            pad_x: 0.0,
            pad_y: 0.0,
            orig_width: width,
            orig_height: height,
            input_width: width,
            input_height: height,
        }
    }

    /// Build a `ModelInfo` for testing profile resolution.
    pub fn make_model_info(
        task: ModelTask,
        format: ModelFormat,
        input_shape: Vec<i64>,
        output_shape: Vec<i64>,
    ) -> ng_gateway_models::domain::prelude::ModelInfo {
        ng_gateway_models::domain::prelude::ModelInfo {
            id: 0,
            model_key: "test_model".to_string(),
            name: "Test Model".to_string(),
            version: "1.0.0".to_string(),
            format,
            task,
            path: "test.onnx".to_string(),
            inputs: Some(TensorDescs(vec![TensorDesc {
                name: "images".to_string(),
                shape: input_shape,
                dtype: TensorDType::Float32,
            }])),
            outputs: Some(TensorDescs(vec![TensorDesc {
                name: "output0".to_string(),
                shape: output_shape,
                dtype: TensorDType::Float32,
            }])),
            labels: Some(Labels(vec![
                "person".to_string(),
                "car".to_string(),
                "dog".to_string(),
            ])),
            default_preprocess: None,
            default_postprocess: None,
            size: 1,
            checksum: "test".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// Float approximate equality.
    pub fn approx_eq(a: f32, b: f32, epsilon: f32) -> bool {
        (a - b).abs() < epsilon
    }
}
