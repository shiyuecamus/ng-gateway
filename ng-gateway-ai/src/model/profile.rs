//! Model profile — binds a model to its specific pre/post processors.
//!
//! When a model is registered, a profile is created (either auto-detected
//! from metadata or explicitly configured by the user). The profile is
//! engine-internal and never exposed to end users.

#[cfg(feature = "engine")]
mod inner {
    use crate::pipeline::{
        annotator::{DefaultFrameAnnotator, FrameAnnotator},
        defaults::{
            DEFAULT_CLASSIFICATION_APPLY_SOFTMAX, DEFAULT_CLASSIFICATION_SMALL_CLASS_FAST_PATH,
            DEFAULT_CLASSIFICATION_TOP_K, DEFAULT_DETECTION_PARALLEL_THRESHOLD,
            DEFAULT_KEYPOINT_COUNT, DEFAULT_KEYPOINT_MAX_DETECTIONS, DEFAULT_LETTERBOX_PAD_VALUE,
            DEFAULT_MAX_DETECTIONS, DEFAULT_NMS_IOU_THRESHOLD, DEFAULT_NMS_PRESCREEN_MULTIPLIER,
            DEFAULT_SEGMENTATION_PARALLEL_MIN_PIXELS,
        },
        postprocess::{
            AnomalyPostProcessor, ClassificationPostProcessor, KeypointPostProcessor, NmsVariant,
            PassthroughPostProcessor, PostProcessor, SegmentationPostProcessor,
            YoloV5PostProcessor, YoloV8PostProcessor,
        },
        preprocess::{
            CenterCropPreProcessor, DirectResizePreProcessor, LetterboxPreProcessor,
            NormalizationParams, PreProcessor,
        },
    };
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::{
        model::{ModelInfo, ModelTask},
        pipeline::{
            ChannelOrder, NmsVariantConfig, NormalizationConfig, NormalizationPreset,
            PostProcessorConfig, PostProcessorType, PreProcessorConfig, ResizeMode,
        },
    };
    use std::sync::Arc;

    /// Shared preprocessor trait object alias.
    type PreProcessorHandle = Arc<dyn PreProcessor>;
    /// Shared postprocessor trait object alias.
    type PostProcessorHandle = Arc<dyn PostProcessor>;
    /// Resolved pre/post processors for one inference stage.
    type StageProcessors = (PreProcessorHandle, PostProcessorHandle);

    /// A model profile binds a model to its pre/post processors.
    pub struct ModelProfile {
        /// Model identifier this profile is bound to.
        pub model_id: String,
        /// Preprocessor instance.
        pub preprocessor: Arc<dyn PreProcessor>,
        /// Postprocessor instance.
        pub postprocessor: Arc<dyn PostProcessor>,
        /// Optional annotation drawer.
        pub annotator: Option<Arc<dyn FrameAnnotator>>,
    }

    /// Auto-detect the best profile for a model based on its metadata.
    ///
    /// Uses model task + output tensor shape as the primary heuristic:
    /// - ObjectDetection + `[1, C, N]` where C < N and C > 5 → YOLOv8 detection
    /// - ObjectDetection + `[1, C, N]` where C < N and C contains keypoints → YOLOv8-Pose
    /// - ObjectDetection + `[1, N, C]` where N > C → YOLOv5
    /// - Classification + `[1, C]` → Classification (softmax + top-K)
    /// - Segmentation + `[1, C, H, W]` → Segmentation (argmax)
    /// - AnomalyDetection → Anomaly (score + heatmap)
    /// - Unknown → DirectResize + Passthrough
    pub fn auto_detect_profile(model_info: &ModelInfo) -> ModelProfile {
        let (pre, post): (Arc<dyn PreProcessor>, Arc<dyn PostProcessor>) = match model_info.task {
            ModelTask::ObjectDetection => {
                if model_info.is_yolov8_output_format() {
                    if is_pose_model(model_info) {
                        (
                            Arc::new(LetterboxPreProcessor::default()),
                            Arc::new(KeypointPostProcessor::default()),
                        )
                    } else {
                        (
                            Arc::new(LetterboxPreProcessor::default()),
                            Arc::new(YoloV8PostProcessor::default()),
                        )
                    }
                } else {
                    (
                        Arc::new(LetterboxPreProcessor::default()),
                        Arc::new(YoloV5PostProcessor::default()),
                    )
                }
            }
            ModelTask::Classification => (
                Arc::new(CenterCropPreProcessor::default()),
                Arc::new(ClassificationPostProcessor::default()),
            ),
            ModelTask::Segmentation => (
                Arc::new(DirectResizePreProcessor {
                    normalize: NormalizationParams::IMAGENET,
                    rgb_order: true,
                }),
                Arc::new(SegmentationPostProcessor::default()),
            ),
            ModelTask::AnomalyDetection => (
                Arc::new(DirectResizePreProcessor {
                    normalize: NormalizationParams::IMAGENET,
                    rgb_order: true,
                }),
                Arc::new(AnomalyPostProcessor::default()),
            ),
            _ => (
                Arc::new(DirectResizePreProcessor::default()),
                Arc::new(PassthroughPostProcessor),
            ),
        };

        ModelProfile {
            model_id: model_info.id.clone(),
            preprocessor: pre,
            postprocessor: post,
            annotator: Some(Arc::new(DefaultFrameAnnotator)),
        }
    }

    /// Resolve effective processors for an inference stage.
    ///
    /// Priority:
    /// 1. Stage override config (`preprocess` / `postprocess`)
    /// 2. Model-level default override (`ModelInfo.default_*`)
    /// 3. Auto-detected profile default
    pub fn resolve_stage_processors(
        model_info: &ModelInfo,
        confidence_threshold: f32,
        nms_iou_threshold: Option<f32>,
        pre_cfg: Option<&PreProcessorConfig>,
        post_cfg: Option<&PostProcessorConfig>,
    ) -> Result<StageProcessors, AiEngineError> {
        let fallback = auto_detect_profile(model_info);
        let preprocess_cfg = pre_cfg.or(model_info.default_preprocess.as_ref());
        let postprocess_cfg = post_cfg.or(model_info.default_postprocess.as_ref());

        let preprocess =
            build_preprocessor(model_info, fallback.preprocessor.name(), preprocess_cfg)?;
        let postprocess = build_postprocessor(
            model_info,
            fallback.postprocessor.name(),
            confidence_threshold,
            nms_iou_threshold,
            postprocess_cfg,
        )?;
        Ok((preprocess, postprocess))
    }

    fn build_preprocessor(
        model_info: &ModelInfo,
        fallback_type: &str,
        cfg: Option<&PreProcessorConfig>,
    ) -> Result<Arc<dyn PreProcessor>, AiEngineError> {
        let mode = cfg
            .and_then(|c| c.resize_mode)
            .map_or_else(|| parse_resize_mode_str(fallback_type), Ok)?;
        let rgb_order = parse_channel_order(cfg.and_then(|c| c.channel_order))?;

        let default_norm = default_normalization_for_mode(model_info, mode)?;
        let normalize =
            resolve_normalization(cfg.and_then(|c| c.normalization.as_ref()), default_norm)?;

        let pre: Arc<dyn PreProcessor> = match mode {
            ResizeMode::Letterbox => Arc::new(LetterboxPreProcessor {
                pad_value: cfg
                    .and_then(|c| c.pad_value)
                    .unwrap_or(DEFAULT_LETTERBOX_PAD_VALUE),
                normalize,
                rgb_order,
            }),
            ResizeMode::CenterCrop => Arc::new(CenterCropPreProcessor {
                normalize,
                rgb_order,
            }),
            ResizeMode::DirectResize => Arc::new(DirectResizePreProcessor {
                normalize,
                rgb_order,
            }),
        };
        Ok(pre)
    }

    fn build_postprocessor(
        _model_info: &ModelInfo,
        fallback_type: &str,
        confidence_threshold: f32,
        nms_iou_threshold: Option<f32>,
        cfg: Option<&PostProcessorConfig>,
    ) -> Result<Arc<dyn PostProcessor>, AiEngineError> {
        let post_type = cfg
            .and_then(|c| c.r#type)
            .map_or_else(|| parse_postprocess_type_str(fallback_type), Ok)?;
        let nms_iou = nms_iou_threshold.unwrap_or(DEFAULT_NMS_IOU_THRESHOLD);
        let nms_variant = parse_nms_variant(
            cfg.and_then(|c| c.nms_variant),
            cfg.and_then(|c| c.soft_nms_sigma),
        )?;

        let post: Arc<dyn PostProcessor> = match post_type {
            PostProcessorType::YoloV8Detection => Arc::new(YoloV8PostProcessor {
                confidence_threshold,
                nms_iou_threshold: nms_iou,
                max_detections: cfg
                    .and_then(|c| c.max_detections)
                    .unwrap_or(DEFAULT_MAX_DETECTIONS),
                nms_variant,
                detection_parallel_threshold: cfg
                    .and_then(|c| c.detection_parallel_threshold)
                    .unwrap_or(DEFAULT_DETECTION_PARALLEL_THRESHOLD),
                nms_prescreen_multiplier: cfg
                    .and_then(|c| c.nms_prescreen_multiplier)
                    .unwrap_or(DEFAULT_NMS_PRESCREEN_MULTIPLIER),
            }),
            PostProcessorType::YoloV5Detection => Arc::new(YoloV5PostProcessor {
                confidence_threshold,
                nms_iou_threshold: nms_iou,
                max_detections: cfg
                    .and_then(|c| c.max_detections)
                    .unwrap_or(DEFAULT_MAX_DETECTIONS),
                nms_variant,
                detection_parallel_threshold: cfg
                    .and_then(|c| c.detection_parallel_threshold)
                    .unwrap_or(DEFAULT_DETECTION_PARALLEL_THRESHOLD),
                nms_prescreen_multiplier: cfg
                    .and_then(|c| c.nms_prescreen_multiplier)
                    .unwrap_or(DEFAULT_NMS_PRESCREEN_MULTIPLIER),
            }),
            PostProcessorType::Classification => Arc::new(ClassificationPostProcessor {
                top_k: cfg
                    .and_then(|c| c.top_k)
                    .unwrap_or(DEFAULT_CLASSIFICATION_TOP_K),
                apply_softmax: cfg
                    .and_then(|c| c.apply_softmax)
                    .unwrap_or(DEFAULT_CLASSIFICATION_APPLY_SOFTMAX),
                min_confidence: confidence_threshold.clamp(0.0, 1.0),
                small_class_fast_path: cfg
                    .and_then(|c| c.classification_small_class_fast_path)
                    .unwrap_or(DEFAULT_CLASSIFICATION_SMALL_CLASS_FAST_PATH),
            }),
            PostProcessorType::Segmentation => Arc::new(SegmentationPostProcessor {
                parallel_min_pixels: cfg
                    .and_then(|c| c.segmentation_parallel_min_pixels)
                    .unwrap_or(DEFAULT_SEGMENTATION_PARALLEL_MIN_PIXELS),
            }),
            PostProcessorType::YoloV8Pose => Arc::new(KeypointPostProcessor {
                confidence_threshold,
                nms_iou_threshold: nms_iou,
                max_detections: cfg
                    .and_then(|c| c.max_detections)
                    .unwrap_or(DEFAULT_KEYPOINT_MAX_DETECTIONS),
                num_keypoints: cfg
                    .and_then(|c| c.num_keypoints)
                    .unwrap_or(DEFAULT_KEYPOINT_COUNT),
            }),
            PostProcessorType::AnomalyDetection => Arc::new(AnomalyPostProcessor {
                anomaly_threshold: cfg
                    .and_then(|c| c.anomaly_threshold)
                    .unwrap_or(confidence_threshold.clamp(0.0, 1.0)),
            }),
            PostProcessorType::Passthrough => Arc::new(PassthroughPostProcessor),
        };
        Ok(post)
    }

    fn parse_channel_order(value: Option<ChannelOrder>) -> Result<bool, AiEngineError> {
        match value.unwrap_or(ChannelOrder::Rgb) {
            ChannelOrder::Rgb => Ok(true),
            ChannelOrder::Bgr => Ok(false),
        }
    }

    fn resolve_normalization(
        cfg: Option<&NormalizationConfig>,
        fallback: NormalizationParams,
    ) -> Result<NormalizationParams, AiEngineError> {
        let Some(cfg) = cfg else {
            return Ok(fallback);
        };

        let preset = cfg.preset.unwrap_or(NormalizationPreset::Yolo);
        if matches!(preset, NormalizationPreset::Custom) {
            let mean = cfg.mean.ok_or_else(|| {
                AiEngineError::PipelineConfigError(
                    "normalization preset 'custom' requires mean".to_string(),
                )
            })?;
            let std = cfg.std.ok_or_else(|| {
                AiEngineError::PipelineConfigError(
                    "normalization preset 'custom' requires std".to_string(),
                )
            })?;
            return Ok(NormalizationParams { mean, std });
        }

        let preset_name = match preset {
            NormalizationPreset::Yolo => "yolo",
            NormalizationPreset::Imagenet => "imagenet",
            NormalizationPreset::Symmetric => "symmetric",
            NormalizationPreset::Custom => "custom",
        };
        NormalizationParams::from_preset(preset_name).ok_or_else(|| {
            AiEngineError::PipelineConfigError(format!(
                "unsupported normalization preset '{preset_name}'"
            ))
        })
    }

    fn default_normalization_for_mode(
        model_info: &ModelInfo,
        mode: ResizeMode,
    ) -> Result<NormalizationParams, AiEngineError> {
        match mode {
            ResizeMode::Letterbox => Ok(NormalizationParams::YOLO),
            ResizeMode::CenterCrop => Ok(NormalizationParams::IMAGENET),
            ResizeMode::DirectResize => {
                if matches!(
                    model_info.task,
                    ModelTask::Segmentation | ModelTask::AnomalyDetection
                ) {
                    Ok(NormalizationParams::IMAGENET)
                } else {
                    Ok(NormalizationParams::YOLO)
                }
            }
        }
    }

    fn parse_nms_variant(
        variant: Option<NmsVariantConfig>,
        soft_sigma: Option<f32>,
    ) -> Result<NmsVariant, AiEngineError> {
        match variant.unwrap_or(NmsVariantConfig::Classic) {
            NmsVariantConfig::Classic => Ok(NmsVariant::Classic),
            NmsVariantConfig::Diou => Ok(NmsVariant::DIoU),
            NmsVariantConfig::Soft => Ok(NmsVariant::Soft {
                sigma: soft_sigma.unwrap_or(0.5).max(1e-6),
            }),
        }
    }

    fn parse_resize_mode_str(value: &str) -> Result<ResizeMode, AiEngineError> {
        match value {
            "letterbox" => Ok(ResizeMode::Letterbox),
            "center_crop" => Ok(ResizeMode::CenterCrop),
            "direct_resize" => Ok(ResizeMode::DirectResize),
            other => Err(AiEngineError::PipelineConfigError(format!(
                "unsupported preprocess.resize_mode '{other}', expected one of: letterbox, center_crop, direct_resize"
            ))),
        }
    }

    fn parse_postprocess_type_str(value: &str) -> Result<PostProcessorType, AiEngineError> {
        match value {
            "yolov8_detection" => Ok(PostProcessorType::YoloV8Detection),
            "yolov5_detection" => Ok(PostProcessorType::YoloV5Detection),
            "classification" => Ok(PostProcessorType::Classification),
            "segmentation" => Ok(PostProcessorType::Segmentation),
            "yolov8_pose" => Ok(PostProcessorType::YoloV8Pose),
            "anomaly_detection" => Ok(PostProcessorType::AnomalyDetection),
            "passthrough" => Ok(PostProcessorType::Passthrough),
            other => Err(AiEngineError::PipelineConfigError(format!(
                "unsupported postprocess.type '{other}'"
            ))),
        }
    }

    /// Heuristic: detect if a YOLOv8-format model is a pose model.
    ///
    /// Pose models output `[1, (5 + K×3), N]` where K=17 for COCO,
    /// giving feature dim = 56. Detection models output `[1, (4+C), N]`
    /// where C is typically 1-80 classes. We check if the feature dimension
    /// matches the pose pattern (5 + 3×K for common K values).
    fn is_pose_model(model_info: &ModelInfo) -> bool {
        if let Some(output) = model_info.outputs.first() {
            if output.shape.len() == 3 {
                let features = output.shape[1];
                // 5 + 17×3 = 56 (COCO), 5 + 13×3 = 44 (MPII-like), 5 + 21×3 = 68 (hand)
                let possible_keypoint_counts = [17, 13, 21, 26, 33];
                return possible_keypoint_counts
                    .iter()
                    .any(|&k| features == (5 + k * 3));
            }
        }
        false
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ng_gateway_models::ai::{
            model::{ModelFormat, TensorDType, TensorDesc},
            pipeline::{NormalizationConfig, PostProcessorConfig, PreProcessorConfig},
        };
        use std::path::PathBuf;

        fn detection_model() -> ModelInfo {
            ModelInfo {
                id: "det_model".to_string(),
                name: "det".to_string(),
                version: "1.0.0".to_string(),
                format: ModelFormat::Onnx,
                path: PathBuf::from("det.onnx"),
                inputs: vec![TensorDesc {
                    name: "images".to_string(),
                    shape: vec![1, 3, 640, 640],
                    dtype: TensorDType::Float32,
                }],
                outputs: vec![TensorDesc {
                    name: "output0".to_string(),
                    shape: vec![1, 84, 8400],
                    dtype: TensorDType::Float32,
                }],
                task: ModelTask::ObjectDetection,
                labels: vec!["person".to_string()],
                default_preprocess: None,
                default_postprocess: None,
                loaded: false,
                file_size: 1,
            }
        }

        #[test]
        fn stage_preprocess_override_takes_effect() {
            let model = detection_model();
            let pre_cfg = PreProcessorConfig {
                resize_mode: Some(ResizeMode::DirectResize),
                normalization: Some(NormalizationConfig {
                    preset: Some(NormalizationPreset::Imagenet),
                    mean: None,
                    std: None,
                }),
                channel_order: Some(ChannelOrder::Bgr),
                pad_value: None,
            };
            let (pre, _post) =
                resolve_stage_processors(&model, 0.5, Some(0.45), Some(&pre_cfg), None)
                    .expect("resolve processors");
            assert_eq!(pre.name(), "direct_resize");
        }

        #[test]
        fn invalid_preprocess_mode_returns_pipeline_error() {
            let pre_cfg_json = serde_json::json!({
                "resize_mode": "invalid_mode"
            });
            let decode = serde_json::from_value::<PreProcessorConfig>(pre_cfg_json);
            assert!(
                decode.is_err(),
                "invalid enum variant must fail deserialization"
            );
        }

        #[test]
        fn postprocess_type_override_takes_effect() {
            let model = detection_model();
            let post_cfg = PostProcessorConfig {
                r#type: Some(PostProcessorType::Passthrough),
                top_k: None,
                apply_softmax: None,
                max_detections: None,
                num_keypoints: None,
                anomaly_threshold: None,
                nms_variant: None,
                soft_nms_sigma: None,
                detection_parallel_threshold: None,
                nms_prescreen_multiplier: None,
                classification_small_class_fast_path: None,
                segmentation_parallel_min_pixels: None,
            };
            let (_pre, post) =
                resolve_stage_processors(&model, 0.5, Some(0.45), None, Some(&post_cfg))
                    .expect("resolve processors");
            assert_eq!(post.name(), "passthrough");
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
