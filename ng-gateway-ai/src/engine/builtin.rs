//! Built-in processor registry — static metadata for available pre/post processors.

use ng_gateway_models::domain::prelude::{ParamType, ProcessorInfo, ProcessorParameter};

/// List of built-in preprocessors and their configurable parameters.
pub(super) fn builtin_preprocessors() -> Vec<ProcessorInfo> {
    vec![
        ProcessorInfo {
            id: "letterbox".into(),
            name: "Letterbox Resize".into(),
            description: "Preserves aspect ratio with padding. Standard for YOLO models.".into(),
            applicable_tasks: vec!["object_detection".into(), "segmentation".into()],
            parameters: vec![
                ProcessorParameter {
                    name: "pad_value".into(),
                    description: "Fill value for padding pixels (0-255).".into(),
                    param_type: ParamType::U8,
                    default: Some(serde_json::json!(114)),
                    required: false,
                },
                ProcessorParameter {
                    name: "normalization".into(),
                    description: "Normalization preset: yolo, imagenet, symmetric, or custom."
                        .into(),
                    param_type: ParamType::String,
                    default: Some(serde_json::json!("yolo")),
                    required: false,
                },
            ],
        },
        ProcessorInfo {
            id: "center_crop".into(),
            name: "Center Crop".into(),
            description: "Crops the center region and resizes. Common for classification models."
                .into(),
            applicable_tasks: vec!["classification".into()],
            parameters: vec![ProcessorParameter {
                name: "normalization".into(),
                description: "Normalization preset: yolo, imagenet, symmetric, or custom.".into(),
                param_type: ParamType::String,
                default: Some(serde_json::json!("imagenet")),
                required: false,
            }],
        },
        ProcessorInfo {
            id: "direct_resize".into(),
            name: "Direct Resize".into(),
            description: "Directly resizes to target dimensions (may distort aspect ratio).".into(),
            applicable_tasks: vec![
                "object_detection".into(),
                "classification".into(),
                "segmentation".into(),
            ],
            parameters: vec![ProcessorParameter {
                name: "normalization".into(),
                description: "Normalization preset: yolo, imagenet, symmetric, or custom.".into(),
                param_type: ParamType::String,
                default: Some(serde_json::json!("yolo")),
                required: false,
            }],
        },
        ProcessorInfo {
            id: "rknn_letterbox".into(),
            name: "RKNN Letterbox (uint8 NHWC)".into(),
            description: "Letterbox resize outputting uint8 NHWC for RKNN quantized models. \
                Skips float normalization and NCHW transpose for maximum NPU throughput. \
                Auto-selected for RKNN format models."
                .into(),
            applicable_tasks: vec![
                "object_detection".into(),
                "classification".into(),
                "segmentation".into(),
            ],
            parameters: vec![ProcessorParameter {
                name: "pad_value".into(),
                description: "Fill value for letterbox padding pixels (0-255).".into(),
                param_type: ParamType::U8,
                default: Some(serde_json::json!(114)),
                required: false,
            }],
        },
    ]
}

/// List of built-in postprocessors and their configurable parameters.
pub(super) fn builtin_postprocessors() -> Vec<ProcessorInfo> {
    vec![
        ProcessorInfo {
            id: "yolov8_detection".into(),
            name: "YOLOv8 Detection".into(),
            description:
                "Post-processes YOLOv8 detection output. Applies confidence thresholding and NMS."
                    .into(),
            applicable_tasks: vec!["object_detection".into()],
            parameters: vec![
                ProcessorParameter {
                    name: "confidence_threshold".into(),
                    description: "Minimum confidence score.".into(),
                    param_type: ParamType::F32,
                    default: Some(serde_json::json!(0.5)),
                    required: false,
                },
                ProcessorParameter {
                    name: "nms_iou_threshold".into(),
                    description: "IoU threshold for NMS.".into(),
                    param_type: ParamType::F32,
                    default: Some(serde_json::json!(0.45)),
                    required: false,
                },
                ProcessorParameter {
                    name: "max_detections".into(),
                    description: "Maximum detections after NMS.".into(),
                    param_type: ParamType::Usize,
                    default: Some(serde_json::json!(300)),
                    required: false,
                },
            ],
        },
        ProcessorInfo {
            id: "yolov5_detection".into(),
            name: "YOLOv5 Detection".into(),
            description:
                "Post-processes YOLOv5 detection output. Applies confidence thresholding and NMS."
                    .into(),
            applicable_tasks: vec!["object_detection".into()],
            parameters: vec![
                ProcessorParameter {
                    name: "confidence_threshold".into(),
                    description: "Minimum confidence score.".into(),
                    param_type: ParamType::F32,
                    default: Some(serde_json::json!(0.5)),
                    required: false,
                },
                ProcessorParameter {
                    name: "nms_iou_threshold".into(),
                    description: "IoU threshold for NMS.".into(),
                    param_type: ParamType::F32,
                    default: Some(serde_json::json!(0.45)),
                    required: false,
                },
                ProcessorParameter {
                    name: "max_detections".into(),
                    description: "Maximum detections after NMS.".into(),
                    param_type: ParamType::Usize,
                    default: Some(serde_json::json!(300)),
                    required: false,
                },
            ],
        },
        ProcessorInfo {
            id: "classification".into(),
            name: "Classification (Softmax + Top-K)".into(),
            description: "Applies softmax and returns top-K class predictions.".into(),
            applicable_tasks: vec!["classification".into()],
            parameters: vec![
                ProcessorParameter {
                    name: "top_k".into(),
                    description: "Number of top predictions.".into(),
                    param_type: ParamType::Usize,
                    default: Some(serde_json::json!(5)),
                    required: false,
                },
                ProcessorParameter {
                    name: "apply_softmax".into(),
                    description: "Whether to apply softmax.".into(),
                    param_type: ParamType::Bool,
                    default: Some(serde_json::json!(true)),
                    required: false,
                },
            ],
        },
        ProcessorInfo {
            id: "segmentation".into(),
            name: "Semantic Segmentation".into(),
            description: "Performs argmax on [1,C,H,W] tensor for per-pixel class mask.".into(),
            applicable_tasks: vec!["segmentation".into()],
            parameters: vec![],
        },
        ProcessorInfo {
            id: "yolov8_pose".into(),
            name: "YOLOv8 Pose / Keypoint Detection".into(),
            description: "Post-processes YOLOv8-Pose output with bbox + keypoints.".into(),
            applicable_tasks: vec!["object_detection".into()],
            parameters: vec![
                ProcessorParameter {
                    name: "confidence_threshold".into(),
                    description: "Minimum confidence score.".into(),
                    param_type: ParamType::F32,
                    default: Some(serde_json::json!(0.5)),
                    required: false,
                },
                ProcessorParameter {
                    name: "nms_iou_threshold".into(),
                    description: "IoU threshold for NMS.".into(),
                    param_type: ParamType::F32,
                    default: Some(serde_json::json!(0.45)),
                    required: false,
                },
                ProcessorParameter {
                    name: "max_detections".into(),
                    description: "Maximum detections after NMS.".into(),
                    param_type: ParamType::Usize,
                    default: Some(serde_json::json!(100)),
                    required: false,
                },
                ProcessorParameter {
                    name: "num_keypoints".into(),
                    description: "Keypoints per detection (17 for COCO).".into(),
                    param_type: ParamType::Usize,
                    default: Some(serde_json::json!(17)),
                    required: false,
                },
            ],
        },
        ProcessorInfo {
            id: "anomaly_detection".into(),
            name: "Anomaly Detection".into(),
            description: "Extracts anomaly score and optional spatial heatmap.".into(),
            applicable_tasks: vec!["anomaly_detection".into()],
            parameters: vec![ProcessorParameter {
                name: "anomaly_threshold".into(),
                description: "Score threshold for anomaly determination.".into(),
                param_type: ParamType::F32,
                default: Some(serde_json::json!(0.5)),
                required: false,
            }],
        },
        ProcessorInfo {
            id: "passthrough".into(),
            name: "Passthrough".into(),
            description: "Returns raw model outputs without processing.".into(),
            applicable_tasks: vec![
                "object_detection".into(),
                "classification".into(),
                "segmentation".into(),
                "anomaly_detection".into(),
            ],
            parameters: vec![],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn builtin_preprocessors_has_four_entries() {
        assert_eq!(builtin_preprocessors().len(), 4);
    }

    #[test]
    fn builtin_postprocessors_has_seven_entries() {
        assert_eq!(builtin_postprocessors().len(), 7);
    }

    #[test]
    fn preprocessor_ids_are_unique() {
        let procs = builtin_preprocessors();
        let ids: HashSet<&str> = procs.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            procs.len(),
            "duplicate preprocessor IDs detected"
        );
    }

    #[test]
    fn postprocessor_ids_are_unique() {
        let procs = builtin_postprocessors();
        let ids: HashSet<&str> = procs.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            procs.len(),
            "duplicate postprocessor IDs detected"
        );
    }

    #[test]
    fn all_processors_have_nonempty_description() {
        for p in builtin_preprocessors()
            .iter()
            .chain(builtin_postprocessors().iter())
        {
            assert!(
                !p.description.is_empty(),
                "processor '{}' has empty description",
                p.id
            );
        }
    }
}
