//! Pipeline execution context — accumulates results as stages execute.

#[cfg(feature = "engine")]
mod inner {
    use crate::decoded_frame::DecodedFrame;
    use ng_gateway_models::ai::types::{
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
}

#[cfg(feature = "engine")]
pub use inner::*;
