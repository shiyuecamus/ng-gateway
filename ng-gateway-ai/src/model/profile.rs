//! Model profile — binds a model to its specific pre/post processors.
//!
//! When a model is registered, a profile is created (either auto-detected
//! from metadata or explicitly configured by the user). The profile is
//! engine-internal and never exposed to end users.

#[cfg(feature = "engine")]
mod inner {
    use crate::pipeline::{
        annotator::{DefaultFrameAnnotator, FrameAnnotator},
        postprocess::{
            AnomalyPostProcessor, ClassificationPostProcessor, KeypointPostProcessor,
            PassthroughPostProcessor, PostProcessor, SegmentationPostProcessor,
            YoloV5PostProcessor, YoloV8PostProcessor,
        },
        preprocess::{
            CenterCropPreProcessor, DirectResizePreProcessor, LetterboxPreProcessor,
            NormalizationParams, PreProcessor,
        },
    };
    use ng_gateway_models::ai::model::{ModelInfo, ModelTask};
    use std::sync::Arc;

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
                Arc::new(SegmentationPostProcessor),
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
}

#[cfg(feature = "engine")]
pub use inner::*;
