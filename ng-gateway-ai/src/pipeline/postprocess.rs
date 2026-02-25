//! Postprocessing trait and built-in implementations.
//!
//! Engine-internal module: NOT exposed to users or drivers.
//! The engine auto-selects the postprocessor based on model task and
//! output tensor shape. Users fine-tune via `PostProcessorConfig` parameters.

#[cfg(feature = "engine")]
mod inner {
    use crate::pipeline::preprocess::CoordinateTransform;
    use ndarray::ArrayD;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::types::{
        AnomalyMap, BoundingBox, Classification, Detection, Keypoint, KeypointDetection,
        SegmentationMask,
    };
    use std::sync::Arc;

    // ── Raw inference output ───────────────────────────────────────

    /// Raw inference output from ONNX Runtime.
    pub struct RawInferenceOutput {
        /// Output tensors keyed by name.
        pub tensors: Vec<(String, ArrayD<f32>)>,
    }

    /// Structured postprocessing result that the pipeline aggregates.
    #[derive(Debug, Clone, Default)]
    pub struct PostprocessOutput {
        pub detections: Vec<Detection>,
        pub classifications: Vec<Classification>,
        /// Keypoint/pose detections (e.g., YOLOv8-Pose).
        pub keypoint_detections: Vec<KeypointDetection>,
        /// Segmentation masks.
        pub segmentation_masks: Vec<SegmentationMask>,
        /// Anomaly detection results.
        pub anomaly_maps: Vec<AnomalyMap>,
        /// Raw key-value outputs (for custom/unknown model types).
        pub custom_outputs: Vec<(String, serde_json::Value)>,
    }

    // ── PostProcessor trait ────────────────────────────────────────

    /// Pluggable postprocessor — transforms raw ONNX output tensors
    /// into structured results.
    pub trait PostProcessor: Send + Sync + 'static {
        fn name(&self) -> &str;

        fn process(
            &self,
            output: &RawInferenceOutput,
            coord_transform: &CoordinateTransform,
            labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError>;
    }

    // ── YOLOv8 Detection ───────────────────────────────────────────

    /// YOLOv8 detection postprocessor.
    ///
    /// Output shape: `[1, 4+num_classes, num_predictions]` (anchor-free).
    pub struct YoloV8PostProcessor {
        pub confidence_threshold: f32,
        pub nms_iou_threshold: f32,
        pub max_detections: usize,
    }

    impl Default for YoloV8PostProcessor {
        fn default() -> Self {
            Self {
                confidence_threshold: 0.5,
                nms_iou_threshold: 0.45,
                max_detections: 300,
            }
        }
    }

    impl PostProcessor for YoloV8PostProcessor {
        fn name(&self) -> &str {
            "yolov8_detection"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            coord_transform: &CoordinateTransform,
            labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = &output.tensors[0].1;
            let shape = tensor.shape();
            if shape.len() != 3 {
                return Err(AiEngineError::PostprocessError(format!(
                    "expected 3D tensor, got {}D",
                    shape.len()
                )));
            }
            let num_features = shape[1];
            let num_preds = shape[2];
            let num_classes = num_features - 4;

            let mut raw: Vec<(Detection, f32)> = Vec::with_capacity(512);

            for i in 0..num_preds {
                let cx = tensor[[0, 0, i]];
                let cy = tensor[[0, 1, i]];
                let w = tensor[[0, 2, i]];
                let h = tensor[[0, 3, i]];

                let mut best_class = 0usize;
                let mut best_score = 0.0f32;
                for c in 0..num_classes {
                    let score = tensor[[0, 4 + c, i]];
                    if score > best_score {
                        best_score = score;
                        best_class = c;
                    }
                }

                if best_score < self.confidence_threshold {
                    continue;
                }

                let x_min = (cx - w / 2.0) / coord_transform.input_width as f32;
                let y_min = (cy - h / 2.0) / coord_transform.input_height as f32;
                let x_max = (cx + w / 2.0) / coord_transform.input_width as f32;
                let y_max = (cy + h / 2.0) / coord_transform.input_height as f32;

                let bbox = coord_transform.map_bbox_to_original(&BoundingBox {
                    x_min,
                    y_min,
                    x_max,
                    y_max,
                });

                let class_label = labels
                    .get(best_class)
                    .map(|s| Arc::<str>::from(s.as_str()))
                    .unwrap_or(Arc::from(format!("class_{best_class}")));

                raw.push((
                    Detection {
                        bbox,
                        class: class_label,
                        class_id: best_class as u32,
                        confidence: best_score,
                        track_id: None,
                    },
                    best_score,
                ));
            }

            let detections = nms_per_class(&mut raw, self.nms_iou_threshold, self.max_detections);
            Ok(PostprocessOutput {
                detections,
                ..Default::default()
            })
        }
    }

    // ── YOLOv5 Detection ───────────────────────────────────────────

    /// YOLOv5 detection postprocessor.
    ///
    /// Output shape: `[1, num_predictions, 4+1+num_classes]`
    /// where the extra `1` is the objectness score.
    pub struct YoloV5PostProcessor {
        pub confidence_threshold: f32,
        pub nms_iou_threshold: f32,
        pub max_detections: usize,
    }

    impl Default for YoloV5PostProcessor {
        fn default() -> Self {
            Self {
                confidence_threshold: 0.5,
                nms_iou_threshold: 0.45,
                max_detections: 300,
            }
        }
    }

    impl PostProcessor for YoloV5PostProcessor {
        fn name(&self) -> &str {
            "yolov5_detection"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            coord_transform: &CoordinateTransform,
            labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = &output.tensors[0].1;
            let shape = tensor.shape();
            if shape.len() != 3 {
                return Err(AiEngineError::PostprocessError(format!(
                    "expected 3D tensor, got {}D",
                    shape.len()
                )));
            }
            let num_preds = shape[1];
            let num_classes = shape[2] - 5;

            let mut raw: Vec<(Detection, f32)> = Vec::with_capacity(512);

            for i in 0..num_preds {
                let objectness = tensor[[0, i, 4]];
                if objectness < self.confidence_threshold {
                    continue;
                }

                let mut best_class = 0usize;
                let mut best_score = 0.0f32;
                for c in 0..num_classes {
                    let score = tensor[[0, i, 5 + c]] * objectness;
                    if score > best_score {
                        best_score = score;
                        best_class = c;
                    }
                }

                if best_score < self.confidence_threshold {
                    continue;
                }

                let cx = tensor[[0, i, 0]];
                let cy = tensor[[0, i, 1]];
                let w = tensor[[0, i, 2]];
                let h = tensor[[0, i, 3]];

                let x_min = (cx - w / 2.0) / coord_transform.input_width as f32;
                let y_min = (cy - h / 2.0) / coord_transform.input_height as f32;
                let x_max = (cx + w / 2.0) / coord_transform.input_width as f32;
                let y_max = (cy + h / 2.0) / coord_transform.input_height as f32;

                let bbox = coord_transform.map_bbox_to_original(&BoundingBox {
                    x_min,
                    y_min,
                    x_max,
                    y_max,
                });

                let class_label = labels
                    .get(best_class)
                    .map(|s| Arc::<str>::from(s.as_str()))
                    .unwrap_or(Arc::from(format!("class_{best_class}")));

                raw.push((
                    Detection {
                        bbox,
                        class: class_label,
                        class_id: best_class as u32,
                        confidence: best_score,
                        track_id: None,
                    },
                    best_score,
                ));
            }

            let detections = nms_per_class(&mut raw, self.nms_iou_threshold, self.max_detections);
            Ok(PostprocessOutput {
                detections,
                ..Default::default()
            })
        }
    }

    // ── Classification ─────────────────────────────────────────────

    /// Classification postprocessor (ResNet, EfficientNet, MobileNet, etc.).
    ///
    /// Output shape: `[1, num_classes]`.
    pub struct ClassificationPostProcessor {
        pub top_k: usize,
        pub apply_softmax: bool,
        pub min_confidence: f32,
    }

    impl Default for ClassificationPostProcessor {
        fn default() -> Self {
            Self {
                top_k: 5,
                apply_softmax: true,
                min_confidence: 0.01,
            }
        }
    }

    impl PostProcessor for ClassificationPostProcessor {
        fn name(&self) -> &str {
            "classification"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            _coord_transform: &CoordinateTransform,
            labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = &output.tensors[0].1;
            let num_classes = tensor.shape()[1];

            let mut scores: Vec<f32> = (0..num_classes).map(|i| tensor[[0, i]]).collect();

            if self.apply_softmax {
                let max_val = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let exp_sum: f32 = scores.iter().map(|&s| (s - max_val).exp()).sum();
                for s in &mut scores {
                    *s = (*s - max_val).exp() / exp_sum;
                }
            }

            let mut indexed: Vec<(usize, f32)> = scores.into_iter().enumerate().collect();
            indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let top_k: Vec<(Arc<str>, f32)> = indexed
                .into_iter()
                .take(self.top_k)
                .filter(|(_, score)| *score >= self.min_confidence)
                .map(|(idx, score)| {
                    let label = labels
                        .get(idx)
                        .map(|s| Arc::<str>::from(s.as_str()))
                        .unwrap_or(Arc::from(format!("class_{idx}")));
                    (label, score)
                })
                .collect();

            Ok(PostprocessOutput {
                classifications: vec![Classification { top_k }],
                ..Default::default()
            })
        }
    }

    // ── Passthrough ────────────────────────────────────────────────

    /// Passthrough postprocessor — returns raw tensor data as JSON.
    ///
    /// Used for unknown/custom model types where the user handles
    /// interpretation via a WASM `ResultProcessor` stage.
    pub struct PassthroughPostProcessor;

    impl PostProcessor for PassthroughPostProcessor {
        fn name(&self) -> &str {
            "passthrough"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            _coord_transform: &CoordinateTransform,
            _labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let custom: Vec<(String, serde_json::Value)> = output
                .tensors
                .iter()
                .map(|(name, tensor)| {
                    let flat: Vec<f32> = tensor.iter().copied().collect();
                    (
                        name.clone(),
                        serde_json::json!({
                            "shape": tensor.shape(),
                            "data": flat,
                        }),
                    )
                })
                .collect();

            Ok(PostprocessOutput {
                custom_outputs: custom,
                ..Default::default()
            })
        }
    }

    // ── Segmentation ──────────────────────────────────────────────

    /// Semantic segmentation postprocessor.
    ///
    /// Output shape: `[1, num_classes, H, W]` (per-pixel class logits).
    /// Performs argmax along the class dimension to produce a per-pixel
    /// class index mask.
    pub struct SegmentationPostProcessor;

    impl PostProcessor for SegmentationPostProcessor {
        fn name(&self) -> &str {
            "segmentation"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            _coord_transform: &CoordinateTransform,
            labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = &output.tensors[0].1;
            let shape = tensor.shape();
            if shape.len() != 4 {
                return Err(AiEngineError::PostprocessError(format!(
                    "segmentation expects 4D tensor [1,C,H,W], got {}D",
                    shape.len()
                )));
            }
            let num_classes = shape[1];
            let h = shape[2];
            let w = shape[3];

            let mut mask = Vec::with_capacity(h * w);
            for y in 0..h {
                for x in 0..w {
                    let mut best_class = 0u8;
                    let mut best_score = f32::NEG_INFINITY;
                    for c in 0..num_classes {
                        let score = tensor[[0, c, y, x]];
                        if score > best_score {
                            best_score = score;
                            best_class = c as u8;
                        }
                    }
                    mask.push(best_class);
                }
            }

            let label_arcs: Vec<Arc<str>> = labels
                .iter()
                .map(|s| Arc::<str>::from(s.as_str()))
                .collect();

            Ok(PostprocessOutput {
                segmentation_masks: vec![SegmentationMask {
                    mask,
                    width: w as u32,
                    height: h as u32,
                    labels: label_arcs,
                }],
                ..Default::default()
            })
        }
    }

    // ── YOLOv8-Pose / Keypoint Detection ────────────────────────

    /// YOLOv8-Pose keypoint detection postprocessor.
    ///
    /// Output shape: `[1, (5 + num_keypoints×3), num_predictions]`
    ///   - 5 = cx, cy, w, h, confidence (box score)
    ///   - For COCO pose: 17 keypoints × 3 (x, y, visibility) = 51
    ///   - Total features per prediction = 56
    ///
    /// Steps:
    /// 1. Extract bbox + confidence + keypoints per prediction
    /// 2. Filter by confidence threshold
    /// 3. Map coordinates back to original frame space
    /// 4. Apply NMS on bounding boxes
    pub struct KeypointPostProcessor {
        pub confidence_threshold: f32,
        pub nms_iou_threshold: f32,
        pub max_detections: usize,
        pub num_keypoints: usize,
    }

    impl Default for KeypointPostProcessor {
        fn default() -> Self {
            Self {
                confidence_threshold: 0.5,
                nms_iou_threshold: 0.45,
                max_detections: 100,
                num_keypoints: 17,
            }
        }
    }

    impl PostProcessor for KeypointPostProcessor {
        fn name(&self) -> &str {
            "yolov8_pose"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            coord_transform: &CoordinateTransform,
            labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = &output.tensors[0].1;
            let shape = tensor.shape();

            if shape.len() != 3 {
                return Err(AiEngineError::PostprocessError(format!(
                    "yolov8_pose expects 3D tensor [1, F, N], got {}D",
                    shape.len()
                )));
            }

            let num_features = shape[1];
            let num_preds = shape[2];
            let expected_features = 5 + self.num_keypoints * 3;

            if num_features < expected_features {
                return Err(AiEngineError::PostprocessError(format!(
                    "yolov8_pose expects >= {} features (5 + {}×3), got {}",
                    expected_features, self.num_keypoints, num_features
                )));
            }

            let class_label = labels
                .first()
                .map(|s| Arc::<str>::from(s.as_str()))
                .unwrap_or(Arc::from("person"));

            let iw = coord_transform.input_width as f32;
            let ih = coord_transform.input_height as f32;

            let mut raw: Vec<(KeypointDetection, f32)> = Vec::with_capacity(256);

            for i in 0..num_preds {
                let cx = tensor[[0, 0, i]];
                let cy = tensor[[0, 1, i]];
                let w = tensor[[0, 2, i]];
                let h = tensor[[0, 3, i]];
                let score = tensor[[0, 4, i]];

                if score < self.confidence_threshold {
                    continue;
                }

                let bbox = coord_transform.map_bbox_to_original(&BoundingBox {
                    x_min: (cx - w / 2.0) / iw,
                    y_min: (cy - h / 2.0) / ih,
                    x_max: (cx + w / 2.0) / iw,
                    y_max: (cy + h / 2.0) / ih,
                });

                let mut keypoints = Vec::with_capacity(self.num_keypoints);
                for k in 0..self.num_keypoints {
                    let kx = tensor[[0, 5 + k * 3, i]];
                    let ky = tensor[[0, 5 + k * 3 + 1, i]];
                    let kv = tensor[[0, 5 + k * 3 + 2, i]];

                    let (ox, oy) = coord_transform.map_point_to_original(kx / iw, ky / ih);
                    keypoints.push(Keypoint {
                        x: ox,
                        y: oy,
                        confidence: kv,
                    });
                }

                raw.push((
                    KeypointDetection {
                        bbox,
                        confidence: score,
                        class: Arc::clone(&class_label),
                        class_id: 0,
                        keypoints,
                        track_id: None,
                    },
                    score,
                ));
            }

            let detections =
                nms_keypoint_detections(&mut raw, self.nms_iou_threshold, self.max_detections);

            Ok(PostprocessOutput {
                keypoint_detections: detections,
                ..Default::default()
            })
        }
    }

    /// Greedy NMS for keypoint detections (class-agnostic since typically single-class).
    fn nms_keypoint_detections(
        raw: &mut [(KeypointDetection, f32)],
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<KeypointDetection> {
        raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut keep = Vec::with_capacity(max_detections);
        let mut suppressed = vec![false; raw.len()];

        for i in 0..raw.len() {
            if suppressed[i] || keep.len() >= max_detections {
                continue;
            }
            keep.push(raw[i].0.clone());

            for j in (i + 1)..raw.len() {
                if suppressed[j] {
                    continue;
                }
                if raw[i].0.bbox.iou(&raw[j].0.bbox) > iou_threshold {
                    suppressed[j] = true;
                }
            }
        }

        keep
    }

    // ── Anomaly Detection ───────────────────────────────────────

    /// Anomaly detection postprocessor.
    ///
    /// Supports two common output formats:
    /// - **Score only**: `[1, 1]` — a single global anomaly score
    /// - **Score + Heatmap**: first output `[1, 1]` (score) + second output
    ///   `[1, 1, H, W]` (spatial anomaly heatmap)
    ///
    /// The score is compared against `anomaly_threshold` to determine
    /// the `is_anomalous` flag.
    pub struct AnomalyPostProcessor {
        /// Threshold for anomaly determination (score above = anomalous).
        pub anomaly_threshold: f32,
    }

    impl Default for AnomalyPostProcessor {
        fn default() -> Self {
            Self {
                anomaly_threshold: 0.5,
            }
        }
    }

    impl PostProcessor for AnomalyPostProcessor {
        fn name(&self) -> &str {
            "anomaly_detection"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            _coord_transform: &CoordinateTransform,
            _labels: &[String],
        ) -> Result<PostprocessOutput, AiEngineError> {
            if output.tensors.is_empty() {
                return Err(AiEngineError::PostprocessError(
                    "anomaly_detection: no output tensors".into(),
                ));
            }

            // Extract global anomaly score from first tensor.
            let score_tensor = &output.tensors[0].1;
            let score = score_tensor
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);

            // Extract spatial heatmap from second tensor (if present).
            let (heatmap, heatmap_w, heatmap_h) = if output.tensors.len() > 1 {
                let hm_tensor = &output.tensors[1].1;
                let hm_shape = hm_tensor.shape();
                if hm_shape.len() == 4 {
                    let h = hm_shape[2];
                    let w = hm_shape[3];
                    let data: Vec<f32> = hm_tensor.iter().copied().collect();
                    (Some(data), w as u32, h as u32)
                } else if hm_shape.len() == 3 {
                    let h = hm_shape[1];
                    let w = hm_shape[2];
                    let data: Vec<f32> = hm_tensor.iter().copied().collect();
                    (Some(data), w as u32, h as u32)
                } else {
                    (None, 0u32, 0u32)
                }
            } else {
                (None, 0u32, 0u32)
            };

            Ok(PostprocessOutput {
                anomaly_maps: vec![AnomalyMap {
                    score,
                    heatmap,
                    heatmap_width: heatmap_w,
                    heatmap_height: heatmap_h,
                    is_anomalous: score >= self.anomaly_threshold,
                    threshold: self.anomaly_threshold,
                }],
                ..Default::default()
            })
        }
    }

    // ── NMS variants ────────────────────────────────────────────

    /// NMS algorithm variant.
    #[derive(Debug, Clone, Copy, Default)]
    pub enum NmsVariant {
        /// Standard greedy NMS: fully suppress overlapping detections.
        #[default]
        Classic,
        /// Soft-NMS: decay confidence of overlapping detections instead of removing.
        /// Parameter is the sigma for Gaussian decay.
        Soft { sigma: f32 },
        /// DIoU-NMS: uses Distance-IoU for more accurate suppression of
        /// overlapping boxes with different aspect ratios.
        DIoU,
    }

    // ── NMS utility ────────────────────────────────────────────────

    /// Greedy NMS per class: suppress overlapping detections with IoU above threshold.
    ///
    /// Uses the classic variant by default. See [`nms_per_class_with_variant`]
    /// for Soft-NMS and DIoU-NMS support.
    pub fn nms_per_class(
        raw: &mut [(Detection, f32)],
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        nms_per_class_with_variant(raw, iou_threshold, max_detections, NmsVariant::Classic)
    }

    /// NMS per class with configurable algorithm variant.
    pub fn nms_per_class_with_variant(
        raw: &mut [(Detection, f32)],
        iou_threshold: f32,
        max_detections: usize,
        variant: NmsVariant,
    ) -> Vec<Detection> {
        raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        match variant {
            NmsVariant::Classic => nms_classic(raw, iou_threshold, max_detections),
            NmsVariant::Soft { sigma } => nms_soft(raw, sigma, iou_threshold, max_detections),
            NmsVariant::DIoU => nms_diou(raw, iou_threshold, max_detections),
        }
    }

    /// Classic greedy NMS: hard-suppress overlapping detections.
    fn nms_classic(
        raw: &[(Detection, f32)],
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        let mut keep = Vec::with_capacity(max_detections);
        let mut suppressed = vec![false; raw.len()];

        for i in 0..raw.len() {
            if suppressed[i] || keep.len() >= max_detections {
                continue;
            }
            keep.push(raw[i].0.clone());

            for j in (i + 1)..raw.len() {
                if suppressed[j] || raw[j].0.class_id != raw[i].0.class_id {
                    continue;
                }
                if raw[i].0.bbox.iou(&raw[j].0.bbox) > iou_threshold {
                    suppressed[j] = true;
                }
            }
        }

        keep
    }

    /// Soft-NMS: instead of hard suppression, decay confidence using
    /// a Gaussian penalty: `score *= exp(-iou² / sigma)`.
    ///
    /// This preserves partially overlapping detections that classic NMS
    /// would discard, improving recall in crowded scenes.
    fn nms_soft(
        raw: &mut [(Detection, f32)],
        sigma: f32,
        score_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        let mut keep = Vec::with_capacity(max_detections);

        for i in 0..raw.len() {
            if keep.len() >= max_detections {
                break;
            }

            // Find the highest-scoring remaining detection
            let mut max_idx = i;
            for j in (i + 1)..raw.len() {
                if raw[j].0.class_id == raw[i].0.class_id && raw[j].1 > raw[max_idx].1 {
                    max_idx = j;
                }
            }
            raw.swap(i, max_idx);

            if raw[i].1 < score_threshold {
                continue;
            }
            keep.push(raw[i].0.clone());

            // Decay scores of overlapping detections
            for j in (i + 1)..raw.len() {
                if raw[j].0.class_id != raw[i].0.class_id {
                    continue;
                }
                let iou = raw[i].0.bbox.iou(&raw[j].0.bbox);
                if iou > 0.0 {
                    let decay = (-iou * iou / sigma).exp();
                    raw[j].1 *= decay;
                    raw[j].0.confidence *= decay;
                }
            }
        }

        keep
    }

    /// DIoU-NMS: uses Distance-IoU instead of standard IoU for suppression.
    ///
    /// DIoU considers both overlap and center-point distance, providing
    /// better suppression for boxes with similar overlap but different
    /// center positions (common with elongated objects).
    fn nms_diou(
        raw: &[(Detection, f32)],
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        let mut keep = Vec::with_capacity(max_detections);
        let mut suppressed = vec![false; raw.len()];

        for i in 0..raw.len() {
            if suppressed[i] || keep.len() >= max_detections {
                continue;
            }
            keep.push(raw[i].0.clone());

            for j in (i + 1)..raw.len() {
                if suppressed[j] || raw[j].0.class_id != raw[i].0.class_id {
                    continue;
                }
                if diou(&raw[i].0.bbox, &raw[j].0.bbox) > iou_threshold {
                    suppressed[j] = true;
                }
            }
        }

        keep
    }

    /// Compute Distance-IoU between two bounding boxes.
    ///
    /// DIoU = IoU - (center_distance² / diagonal_distance²)
    /// where diagonal_distance is the diagonal of the smallest enclosing box.
    #[inline]
    fn diou(a: &BoundingBox, b: &BoundingBox) -> f32 {
        let iou = a.iou(b);

        let a_cx = (a.x_min + a.x_max) / 2.0;
        let a_cy = (a.y_min + a.y_max) / 2.0;
        let b_cx = (b.x_min + b.x_max) / 2.0;
        let b_cy = (b.y_min + b.y_max) / 2.0;

        let center_dist_sq = (a_cx - b_cx).powi(2) + (a_cy - b_cy).powi(2);

        // Smallest enclosing box diagonal
        let enclose_x1 = a.x_min.min(b.x_min);
        let enclose_y1 = a.y_min.min(b.y_min);
        let enclose_x2 = a.x_max.max(b.x_max);
        let enclose_y2 = a.y_max.max(b.y_max);
        let diag_sq = (enclose_x2 - enclose_x1).powi(2) + (enclose_y2 - enclose_y1).powi(2);

        if diag_sq <= 0.0 {
            iou
        } else {
            iou - center_dist_sq / diag_sq
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
