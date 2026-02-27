//! Shared default values for pipeline processors.
//!
//! These defaults are intentionally centralized to avoid value drift between
//! processor `Default` implementations and model-profile config resolution.

/// Default confidence threshold used by detection/keypoint/anomaly processors.
pub(crate) const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 0.5;
/// Default IoU threshold used by NMS-based postprocessors.
pub(crate) const DEFAULT_NMS_IOU_THRESHOLD: f32 = 0.45;
/// Default maximum detection count after NMS for generic detection models.
pub(crate) const DEFAULT_MAX_DETECTIONS: usize = 300;
/// Default maximum detection count for keypoint/pose models.
pub(crate) const DEFAULT_KEYPOINT_MAX_DETECTIONS: usize = 100;
/// Default keypoint count for COCO-style pose models.
pub(crate) const DEFAULT_KEYPOINT_COUNT: usize = 17;
/// Default top-k output count for classification postprocessing.
pub(crate) const DEFAULT_CLASSIFICATION_TOP_K: usize = 5;
/// Default softmax switch for classification postprocessing.
pub(crate) const DEFAULT_CLASSIFICATION_APPLY_SOFTMAX: bool = true;
/// Default prediction-count threshold to enable parallel candidate generation.
pub(crate) const DEFAULT_DETECTION_PARALLEL_THRESHOLD: usize = 8_192;
/// Default chunk size used by the parallel YOLOv5 candidate extraction path.
pub(crate) const DEFAULT_DETECTION_PARALLEL_CHUNK_SIZE: usize = 512;
/// Default candidate prescreen multiplier before NMS.
pub(crate) const DEFAULT_NMS_PRESCREEN_MULTIPLIER: usize = 8;
/// Default class-count threshold for classification small-input fast path.
pub(crate) const DEFAULT_CLASSIFICATION_SMALL_CLASS_FAST_PATH: usize = 16;
/// Default pixel threshold to enable segmentation argmax parallelism.
pub(crate) const DEFAULT_SEGMENTATION_PARALLEL_MIN_PIXELS: usize = 128 * 128;
/// Default letterbox padding value used by YOLO-family preprocessing.
pub(crate) const DEFAULT_LETTERBOX_PAD_VALUE: u8 = 114;
/// Default anomaly threshold when no override is supplied.
pub(crate) const DEFAULT_ANOMALY_THRESHOLD: f32 = 0.5;
