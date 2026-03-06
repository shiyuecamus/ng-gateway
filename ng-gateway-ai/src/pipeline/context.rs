//! Pipeline execution context — accumulates results as stages execute.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded::DecodedFrame;
    use ng_gateway_models::domain::prelude::{
        AnomalyMap, Classification, Detection, KeypointDetection, SegmentationMask,
    };

    /// Mutable context that flows through pipeline stages, accumulating results.
    pub struct PipelineContext {
        /// Current frame (may be modified by FrameTransform stages).
        pub current_frame: DecodedFrame,
        /// Accumulated detections from inference stages.
        pub detections: Vec<Detection>,
        /// Accumulated classifications from inference stages.
        pub classifications: Vec<Classification>,
        /// Accumulated keypoint/pose detections.
        pub keypoint_detections: Vec<KeypointDetection>,
        /// Accumulated segmentation masks.
        pub segmentation_masks: Vec<SegmentationMask>,
        /// Accumulated anomaly detection results.
        pub anomaly_maps: Vec<AnomalyMap>,
        /// Custom key-value outputs from passthrough/WASM stages.
        pub custom_outputs: Vec<(String, serde_json::Value)>,
    }

    impl PipelineContext {
        /// Create a new context from a decoded frame.
        pub fn new(frame: DecodedFrame) -> Self {
            Self {
                current_frame: frame,
                detections: Vec::new(),
                classifications: Vec::new(),
                keypoint_detections: Vec::new(),
                segmentation_masks: Vec::new(),
                anomaly_maps: Vec::new(),
                custom_outputs: Vec::new(),
            }
        }

        /// Create a merge-only context that accumulates results from ROI sub-contexts.
        ///
        /// Uses a zero-sized placeholder frame since the merge context never
        /// feeds pixel data into any stage — it only collects results.
        pub fn new_merge_only() -> Self {
            Self {
                current_frame: DecodedFrame::empty(),
                detections: Vec::new(),
                classifications: Vec::new(),
                keypoint_detections: Vec::new(),
                segmentation_masks: Vec::new(),
                anomaly_maps: Vec::new(),
                custom_outputs: Vec::new(),
            }
        }

        /// Extend detections from an inference stage.
        pub fn add_detections(&mut self, detections: Vec<Detection>) {
            self.detections.extend(detections);
        }

        /// Extend classifications from an inference stage.
        pub fn add_classifications(&mut self, classifications: Vec<Classification>) {
            self.classifications.extend(classifications);
        }

        /// Extend keypoint detections from a pose inference stage.
        pub fn add_keypoint_detections(&mut self, detections: Vec<KeypointDetection>) {
            self.keypoint_detections.extend(detections);
        }

        /// Extend segmentation masks from a segmentation inference stage.
        pub fn add_segmentation_masks(&mut self, masks: Vec<SegmentationMask>) {
            self.segmentation_masks.extend(masks);
        }

        /// Extend anomaly maps from an anomaly detection inference stage.
        pub fn add_anomaly_maps(&mut self, maps: Vec<AnomalyMap>) {
            self.anomaly_maps.extend(maps);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::test_utils::make_solid_frame;
        use ng_gateway_models::domain::prelude::*;
        use std::sync::Arc;

        #[test]
        fn context_new_all_fields_empty() {
            let frame = make_solid_frame(64, 48, 0, 0, 0);
            let ctx = PipelineContext::new(frame);

            assert!(ctx.detections.is_empty());
            assert!(ctx.classifications.is_empty());
            assert!(ctx.keypoint_detections.is_empty());
            assert!(ctx.segmentation_masks.is_empty());
            assert!(ctx.anomaly_maps.is_empty());
            assert!(ctx.custom_outputs.is_empty());
        }

        #[test]
        fn add_detections_accumulates() {
            let frame = make_solid_frame(32, 32, 128, 128, 128);
            let mut ctx = PipelineContext::new(frame);

            let batch_a: Vec<Detection> = (0..2)
                .map(|i| Detection {
                    bbox: BoundingBox {
                        x_min: 0.0,
                        y_min: 0.0,
                        x_max: 0.5,
                        y_max: 0.5,
                    },
                    class: Arc::from("obj"),
                    class_id: i,
                    confidence: 0.9,
                    track_id: None,
                })
                .collect();

            let batch_b: Vec<Detection> = (0..3)
                .map(|i| Detection {
                    bbox: BoundingBox {
                        x_min: 0.1,
                        y_min: 0.1,
                        x_max: 0.6,
                        y_max: 0.6,
                    },
                    class: Arc::from("car"),
                    class_id: 10 + i,
                    confidence: 0.8,
                    track_id: None,
                })
                .collect();

            ctx.add_detections(batch_a);
            ctx.add_detections(batch_b);

            assert_eq!(ctx.detections.len(), 5);
        }

        #[test]
        fn merge_only_has_empty_frame() {
            let ctx = PipelineContext::new_merge_only();

            assert_eq!(ctx.current_frame.width, 0);
            assert_eq!(ctx.current_frame.height, 0);
            assert!(ctx.detections.is_empty());
            assert!(ctx.classifications.is_empty());
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
