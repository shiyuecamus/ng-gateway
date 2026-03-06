//! Integration tests: exhaustive `infer_task_and_variant` shape matrix.
//!
//! Parametrically tests a wide variety of output tensor shapes against
//! the shape inference function, verifying the correct (task, variant,
//! postprocessor) tuple for each shape category.

#![cfg(feature = "engine")]

use ng_gateway_ai::model::prober::infer_task_and_variant;
use ng_gateway_models::{
    domain::prelude::ModelVariant,
    entities::ai::model::TensorDesc,
    enums::ai::{ModelTask, PostProcessorType, TensorDType},
};

// ── Helpers ──────────────────────────────────────────────────────────

fn tensor_desc(name: &str, shape: Vec<i64>) -> TensorDesc {
    TensorDesc {
        name: name.to_string(),
        shape,
        dtype: TensorDType::Float32,
    }
}

/// Shape test case for the parametric matrix.
struct ShapeTestCase {
    label: &'static str,
    shape: Vec<i64>,
    nchw: bool,
    expected_task: Option<ModelTask>,
    expected_variant: Option<ModelVariant>,
    expected_post: Option<PostProcessorType>,
}

// ── Test: exhaustive shape inference matrix ───────────────────────────

#[test]
fn shape_inference_matrix() {
    let cases = vec![
        // ── YOLOv8 detection shapes ──────────────────────────────
        ShapeTestCase {
            label: "YOLOv8 COCO (80 classes)",
            shape: vec![1, 84, 8400],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV8),
            expected_post: Some(PostProcessorType::YoloV8Detection),
        },
        ShapeTestCase {
            label: "YOLOv8 single class",
            shape: vec![1, 5, 8400],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV8),
            expected_post: Some(PostProcessorType::YoloV8Detection),
        },
        ShapeTestCase {
            label: "YOLOv8 large (100 classes): 104 > 10 && (104-5)%3 == 0 → pose",
            shape: vec![1, 104, 8400],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV8Pose),
            expected_post: Some(PostProcessorType::YoloV8Pose),
        },
        ShapeTestCase {
            label: "YOLOv8 large (99 classes): (103-5)%3 == 2 → detection",
            shape: vec![1, 103, 8400],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV8),
            expected_post: Some(PostProcessorType::YoloV8Detection),
        },
        ShapeTestCase {
            label: "YOLOv8 nano (fewer predictions)",
            shape: vec![1, 84, 2100],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV8),
            expected_post: Some(PostProcessorType::YoloV8Detection),
        },
        // ── YOLOv8-Pose shapes ───────────────────────────────────
        ShapeTestCase {
            label: "YOLOv8 Pose COCO (17 keypoints: 5 + 17*3 = 56)",
            shape: vec![1, 56, 8400],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV8Pose),
            expected_post: Some(PostProcessorType::YoloV8Pose),
        },
        ShapeTestCase {
            label: "YOLOv8 Pose (6 keypoints: 5 + 6*3 = 23)",
            shape: vec![1, 23, 8400],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV8Pose),
            expected_post: Some(PostProcessorType::YoloV8Pose),
        },
        // ── YOLOv5 detection shapes ──────────────────────────────
        ShapeTestCase {
            label: "YOLOv5 COCO (80 classes)",
            shape: vec![1, 25200, 85],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV5),
            expected_post: Some(PostProcessorType::YoloV5Detection),
        },
        ShapeTestCase {
            label: "YOLOv5 single class",
            shape: vec![1, 25200, 6],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV5),
            expected_post: Some(PostProcessorType::YoloV5Detection),
        },
        ShapeTestCase {
            label: "YOLOv5s6 (large stride grid)",
            shape: vec![1, 18900, 85],
            nchw: true,
            expected_task: Some(ModelTask::ObjectDetection),
            expected_variant: Some(ModelVariant::YoloV5),
            expected_post: Some(PostProcessorType::YoloV5Detection),
        },
        // ── Classification shapes ────────────────────────────────
        ShapeTestCase {
            label: "ImageNet 1000-class classification",
            shape: vec![1, 1000],
            nchw: true,
            expected_task: Some(ModelTask::Classification),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Classification),
        },
        ShapeTestCase {
            label: "Binary classification",
            shape: vec![1, 2],
            nchw: true,
            expected_task: Some(ModelTask::Classification),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Classification),
        },
        ShapeTestCase {
            label: "CIFAR-10 classification",
            shape: vec![1, 10],
            nchw: true,
            expected_task: Some(ModelTask::Classification),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Classification),
        },
        ShapeTestCase {
            label: "CIFAR-100 classification",
            shape: vec![1, 100],
            nchw: true,
            expected_task: Some(ModelTask::Classification),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Classification),
        },
        // ── Segmentation shapes (NCHW) ───────────────────────────
        ShapeTestCase {
            label: "DeepLabV3 VOC (21 classes, 512x512)",
            shape: vec![1, 21, 512, 512],
            nchw: true,
            expected_task: Some(ModelTask::Segmentation),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Segmentation),
        },
        ShapeTestCase {
            label: "Cityscapes (19 classes, 1024x2048)",
            shape: vec![1, 19, 1024, 2048],
            nchw: true,
            expected_task: Some(ModelTask::Segmentation),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Segmentation),
        },
        ShapeTestCase {
            label: "ADE20K (150 classes, 512x512)",
            shape: vec![1, 150, 512, 512],
            nchw: true,
            expected_task: Some(ModelTask::Segmentation),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Segmentation),
        },
        ShapeTestCase {
            label: "Binary segmentation (2 classes)",
            shape: vec![1, 2, 256, 256],
            nchw: true,
            expected_task: Some(ModelTask::Segmentation),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Segmentation),
        },
        // ── Segmentation shapes (NHWC) ───────────────────────────
        ShapeTestCase {
            label: "RKNN segmentation NHWC (21 classes)",
            shape: vec![1, 512, 512, 21],
            nchw: false,
            expected_task: Some(ModelTask::Segmentation),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Segmentation),
        },
        ShapeTestCase {
            label: "RKNN segmentation NHWC (19 classes)",
            shape: vec![1, 256, 256, 19],
            nchw: false,
            expected_task: Some(ModelTask::Segmentation),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::Segmentation),
        },
        // ── Anomaly detection shapes ─────────────────────────────
        ShapeTestCase {
            label: "Anomaly heatmap NCHW (1 channel)",
            shape: vec![1, 1, 256, 256],
            nchw: true,
            expected_task: Some(ModelTask::AnomalyDetection),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::AnomalyDetection),
        },
        ShapeTestCase {
            label: "Anomaly heatmap NCHW (1 channel, 64x64)",
            shape: vec![1, 1, 64, 64],
            nchw: true,
            expected_task: Some(ModelTask::AnomalyDetection),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::AnomalyDetection),
        },
        ShapeTestCase {
            label: "Anomaly heatmap NHWC (1 channel)",
            shape: vec![1, 256, 256, 1],
            nchw: false,
            expected_task: Some(ModelTask::AnomalyDetection),
            expected_variant: Some(ModelVariant::Generic),
            expected_post: Some(PostProcessorType::AnomalyDetection),
        },
        // ── Edge cases ───────────────────────────────────────────
        ShapeTestCase {
            label: "Empty outputs",
            shape: vec![],
            nchw: true,
            expected_task: None,
            expected_variant: None,
            expected_post: None,
        },
        ShapeTestCase {
            label: "Unknown 5D shape",
            shape: vec![1, 2, 3, 4, 5],
            nchw: true,
            expected_task: None,
            expected_variant: Some(ModelVariant::Generic),
            expected_post: None,
        },
        ShapeTestCase {
            label: "Unknown 1D shape",
            shape: vec![100],
            nchw: true,
            expected_task: None,
            expected_variant: Some(ModelVariant::Generic),
            expected_post: None,
        },
        ShapeTestCase {
            label: "Unknown 6D shape",
            shape: vec![1, 2, 3, 4, 5, 6],
            nchw: true,
            expected_task: None,
            expected_variant: Some(ModelVariant::Generic),
            expected_post: None,
        },
    ];

    for case in &cases {
        let outputs = if case.shape.is_empty() {
            vec![]
        } else {
            vec![tensor_desc("output0", case.shape.clone())]
        };

        let (task, variant, post) = infer_task_and_variant(&outputs, case.nchw);

        assert_eq!(
            task, case.expected_task,
            "[{}] task mismatch: expected {:?}, got {:?}",
            case.label, case.expected_task, task
        );
        assert_eq!(
            variant, case.expected_variant,
            "[{}] variant mismatch: expected {:?}, got {:?}",
            case.label, case.expected_variant, variant
        );
        assert_eq!(
            post, case.expected_post,
            "[{}] postprocessor mismatch: expected {:?}, got {:?}",
            case.label, case.expected_post, post
        );
    }
}

// ── Test: NCHW vs NHWC layout consistency ────────────────────────────

#[test]
fn nchw_vs_nhwc_segmentation_consistency() {
    // Same logical segmentation model expressed in both layouts should
    // produce the same task and postprocessor inference.
    let nchw_shape = vec![1, 21, 512, 512];
    let nhwc_shape = vec![1, 512, 512, 21];

    let (task_nchw, _, post_nchw) =
        infer_task_and_variant(&[tensor_desc("output0", nchw_shape)], true);
    let (task_nhwc, _, post_nhwc) =
        infer_task_and_variant(&[tensor_desc("output0", nhwc_shape)], false);

    assert_eq!(task_nchw, task_nhwc, "NCHW and NHWC should infer same task");
    assert_eq!(
        post_nchw, post_nhwc,
        "NCHW and NHWC should infer same postprocessor"
    );
}

// ── Test: NCHW vs NHWC anomaly consistency ───────────────────────────

#[test]
fn nchw_vs_nhwc_anomaly_consistency() {
    let nchw_shape = vec![1, 1, 128, 128];
    let nhwc_shape = vec![1, 128, 128, 1];

    let (task_nchw, _, post_nchw) =
        infer_task_and_variant(&[tensor_desc("output0", nchw_shape)], true);
    let (task_nhwc, _, post_nhwc) =
        infer_task_and_variant(&[tensor_desc("output0", nhwc_shape)], false);

    assert_eq!(task_nchw, task_nhwc);
    assert_eq!(post_nchw, post_nhwc);
    assert_eq!(task_nchw, Some(ModelTask::AnomalyDetection));
}

// ── Test: Pose shape edge boundary ───────────────────────────────────

#[test]
fn pose_shape_edge_boundary() {
    // dim1=11 → 11 > 10 && (11-5) % 3 == 0 → 6/3=2 → pose with 2 keypoints.
    let outputs = vec![tensor_desc("output0", vec![1, 11, 8400])];
    let (task, variant, post) = infer_task_and_variant(&outputs, true);
    assert_eq!(task, Some(ModelTask::ObjectDetection));
    assert_eq!(variant, Some(ModelVariant::YoloV8Pose));
    assert_eq!(post, Some(PostProcessorType::YoloV8Pose));

    // dim1=10 → 10 <= 10 → NOT pose, just YOLOv8 detection (6 classes).
    let outputs = vec![tensor_desc("output0", vec![1, 10, 8400])];
    let (task, variant, _) = infer_task_and_variant(&outputs, true);
    assert_eq!(task, Some(ModelTask::ObjectDetection));
    assert_eq!(variant, Some(ModelVariant::YoloV8));

    // dim1=14 → 14 > 10, (14-5) % 3 == 0 → 9/3=3 → pose with 3 keypoints.
    let outputs = vec![tensor_desc("output0", vec![1, 14, 8400])];
    let (task, variant, post) = infer_task_and_variant(&outputs, true);
    assert_eq!(task, Some(ModelTask::ObjectDetection));
    assert_eq!(variant, Some(ModelVariant::YoloV8Pose));
    assert_eq!(post, Some(PostProcessorType::YoloV8Pose));

    // dim1=12 → 12 > 10, (12-5) % 3 == 2 → NOT pose, YOLOv8 detection.
    let outputs = vec![tensor_desc("output0", vec![1, 12, 8400])];
    let (task, variant, _) = infer_task_and_variant(&outputs, true);
    assert_eq!(task, Some(ModelTask::ObjectDetection));
    assert_eq!(variant, Some(ModelVariant::YoloV8));
}
