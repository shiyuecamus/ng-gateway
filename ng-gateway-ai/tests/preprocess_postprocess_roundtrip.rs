//! Integration tests: full preprocess → mock inference → postprocess pipeline.
//!
//! Verifies that synthetic frames pass through the preprocessing stage,
//! produce well-shaped tensors, and that mock inference outputs decode
//! into structurally valid postprocessing results.

#![cfg(feature = "engine")]

use bytes::Bytes;
use ndarray::{Array3, ArrayD};
use ng_gateway_ai::pipeline::postprocess::*;
use ng_gateway_ai::pipeline::preprocess::*;
use ng_gateway_ai::DecodedFrame;
use ng_gateway_models::enums::ai::TensorDType;
use std::sync::Arc;

// ── Helpers (inlined because test_utils is crate-private) ────────────

/// Create a solid-color RGB24 `DecodedFrame`.
fn make_solid_frame(width: u32, height: u32, r: u8, g: u8, b: u8) -> DecodedFrame {
    let pixel_count = width as usize * height as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for _ in 0..pixel_count {
        data.extend_from_slice(&[r, g, b]);
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

fn labels(n: usize) -> Vec<Arc<str>> {
    (0..n).map(|i| Arc::from(format!("class_{i}"))).collect()
}

// ── Test 1: Letterbox → YOLOv8 → valid detections ────────────────────

#[test]
fn letterbox_then_yolov8_produces_valid_detections() {
    let frame = make_solid_frame(1920, 1080, 128, 128, 128);
    let preprocessor = LetterboxPreProcessor::default();
    let model_shape: [i64; 4] = [1, 3, 640, 640];

    let preprocess_output = preprocessor
        .process(PreprocessInput {
            frame: &frame,
            model_input_shape: &model_shape,
            model_input_dtype: TensorDType::Float32,
        })
        .expect("letterbox preprocess should succeed");

    let coord_transform = *preprocess_output.coord_transform();

    // Construct synthetic YOLOv8 output: [1, 4+num_classes, num_preds].
    // Place one detection at pixel (320, 320) with size 100x100, class_0 score = 0.92.
    let num_classes = 3;
    let num_preds = 1;
    let num_features = 4 + num_classes;
    let mut tensor = Array3::<f32>::zeros((1, num_features, num_preds));
    tensor[[0, 0, 0]] = 320.0; // cx
    tensor[[0, 1, 0]] = 320.0; // cy
    tensor[[0, 2, 0]] = 100.0; // w
    tensor[[0, 3, 0]] = 100.0; // h
    tensor[[0, 4, 0]] = 0.92; // class_0 score
    tensor[[0, 5, 0]] = 0.05; // class_1 score
    tensor[[0, 6, 0]] = 0.03; // class_2 score

    let raw_output = RawInferenceOutput {
        tensors: vec![("output0".into(), tensor.into_dyn())],
    };

    let postprocessor = YoloV8PostProcessor {
        confidence_threshold: 0.5,
        ..YoloV8PostProcessor::default()
    };

    let result = postprocessor
        .process(&raw_output, &coord_transform, &labels(num_classes))
        .expect("yolov8 postprocess should succeed");

    assert_eq!(result.detections.len(), 1, "expected exactly 1 detection");

    let det = &result.detections[0];
    assert!(
        det.bbox.x_min >= 0.0 && det.bbox.x_min <= 1.0,
        "x_min out of [0,1]: {}",
        det.bbox.x_min
    );
    assert!(
        det.bbox.y_min >= 0.0 && det.bbox.y_min <= 1.0,
        "y_min out of [0,1]: {}",
        det.bbox.y_min
    );
    assert!(
        det.bbox.x_max >= 0.0 && det.bbox.x_max <= 1.0,
        "x_max out of [0,1]: {}",
        det.bbox.x_max
    );
    assert!(
        det.bbox.y_max >= 0.0 && det.bbox.y_max <= 1.0,
        "y_max out of [0,1]: {}",
        det.bbox.y_max
    );
    assert!(
        det.bbox.x_max > det.bbox.x_min,
        "bbox width must be positive"
    );
    assert!(
        det.bbox.y_max > det.bbox.y_min,
        "bbox height must be positive"
    );
    assert_eq!(det.class.as_ref(), "class_0");
    assert!(det.confidence > 0.9, "confidence should be > 0.9");
}

// ── Test 2: CenterCrop → Classification → sorted top_k ──────────────

#[test]
fn center_crop_then_classification_produces_valid_output() {
    let frame = make_solid_frame(1920, 1080, 64, 128, 200);
    let preprocessor = CenterCropPreProcessor::default();
    let model_shape: [i64; 4] = [1, 3, 224, 224];

    let preprocess_output = preprocessor
        .process(PreprocessInput {
            frame: &frame,
            model_input_shape: &model_shape,
            model_input_dtype: TensorDType::Float32,
        })
        .expect("center crop preprocess should succeed");

    let coord_transform = *preprocess_output.coord_transform();

    // Tensor shape check.
    let PreprocessOutput::CpuTensor { ref tensor, .. } = preprocess_output else {
        panic!("expected CpuTensor variant from CenterCropPreProcessor");
    };
    assert_eq!(tensor.shape(), &[1, 3, 224, 224]);

    // Synthetic classification output: [1, 1000] with known peaks.
    let num_classes = 1000;
    let mut scores = vec![0.001f32; num_classes];
    scores[42] = 5.0; // highest logit
    scores[100] = 3.5;
    scores[777] = 4.0;
    scores[999] = 2.0;

    let class_tensor =
        ArrayD::from_shape_vec(vec![1, num_classes], scores).expect("shape mismatch");
    let raw_output = RawInferenceOutput {
        tensors: vec![("output0".into(), class_tensor)],
    };

    let postprocessor = ClassificationPostProcessor {
        top_k: 5,
        apply_softmax: true,
        min_confidence: 0.0,
        ..ClassificationPostProcessor::default()
    };

    let result = postprocessor
        .process(&raw_output, &coord_transform, &labels(num_classes))
        .expect("classification postprocess should succeed");

    assert_eq!(result.classifications.len(), 1);
    let top_k = &result.classifications[0].top_k;
    assert!(!top_k.is_empty(), "top_k must not be empty");

    // Verify descending order of scores.
    for window in top_k.windows(2) {
        assert!(
            window[0].1 >= window[1].1,
            "top_k scores must be descending: {} >= {}",
            window[0].1,
            window[1].1
        );
    }

    // The top result should be class_42 (highest logit).
    assert_eq!(
        top_k[0].0.as_ref(),
        "class_42",
        "highest logit class_42 should be rank 1"
    );
}

// ── Test 3: DirectResize → Segmentation → valid mask ─────────────────

#[test]
fn direct_resize_then_segmentation_produces_valid_mask() {
    let frame = make_solid_frame(1920, 1080, 200, 100, 50);
    let preprocessor = DirectResizePreProcessor::default();
    let model_shape: [i64; 4] = [1, 3, 512, 512];

    let preprocess_output = preprocessor
        .process(PreprocessInput {
            frame: &frame,
            model_input_shape: &model_shape,
            model_input_dtype: TensorDType::Float32,
        })
        .expect("direct resize preprocess should succeed");

    let coord_transform = *preprocess_output.coord_transform();

    let PreprocessOutput::CpuTensor { ref tensor, .. } = preprocess_output else {
        panic!("expected CpuTensor variant from DirectResizePreProcessor");
    };
    assert_eq!(tensor.shape(), &[1, 3, 512, 512]);

    // Synthetic segmentation output: [1, 21, 512, 512].
    // Fill with class 0 everywhere, then set a stripe to class 5.
    let num_classes = 21;
    let h = 512;
    let w = 512;
    let hw = h * w;
    let mut seg_data = vec![0.0f32; num_classes * hw];
    // Class 0 has a baseline logit of 1.0 everywhere.
    for val in seg_data.iter_mut().take(hw) {
        *val = 1.0;
    }
    // Class 5 dominates in rows 100..200.
    for y in 100..200 {
        for x in 0..w {
            let p = y * w + x;
            seg_data[5 * hw + p] = 5.0;
        }
    }

    let seg_tensor =
        ArrayD::from_shape_vec(vec![1, num_classes, h, w], seg_data).expect("shape mismatch");
    let raw_output = RawInferenceOutput {
        tensors: vec![("output0".into(), seg_tensor)],
    };

    let postprocessor = SegmentationPostProcessor::default();
    let result = postprocessor
        .process(&raw_output, &coord_transform, &labels(num_classes))
        .expect("segmentation postprocess should succeed");

    assert_eq!(result.segmentation_masks.len(), 1);
    let mask = &result.segmentation_masks[0];
    assert_eq!(mask.width, w as u32);
    assert_eq!(mask.height, h as u32);
    assert_eq!(mask.mask.len(), hw);

    // Verify the class-5 stripe.
    for y in 100..200 {
        for x in 0..w {
            assert_eq!(
                mask.mask[y * w + x],
                5,
                "pixel ({x}, {y}) should be class 5"
            );
        }
    }

    // Verify a pixel outside the stripe is class 0.
    assert_eq!(
        mask.mask[50 * w + 50],
        0,
        "pixel (50, 50) should be class 0"
    );
}
