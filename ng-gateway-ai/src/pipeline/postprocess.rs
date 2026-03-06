//! Postprocessing trait and built-in implementations.
//!
//! Engine-internal module: NOT exposed to users or drivers.
//! The engine auto-selects the postprocessor based on model task and
//! output tensor shape. Users fine-tune via `PostProcessorConfig` parameters.

#[cfg(feature = "engine")]
mod inner {
    use crate::pipeline::defaults::{
        DEFAULT_ANOMALY_THRESHOLD, DEFAULT_CLASSIFICATION_APPLY_SOFTMAX,
        DEFAULT_CLASSIFICATION_SMALL_CLASS_FAST_PATH, DEFAULT_CLASSIFICATION_TOP_K,
        DEFAULT_CONFIDENCE_THRESHOLD, DEFAULT_DETECTION_PARALLEL_CHUNK_SIZE,
        DEFAULT_DETECTION_PARALLEL_THRESHOLD, DEFAULT_KEYPOINT_COUNT,
        DEFAULT_KEYPOINT_MAX_DETECTIONS, DEFAULT_MAX_DETECTIONS, DEFAULT_NMS_IOU_THRESHOLD,
        DEFAULT_NMS_PRESCREEN_MULTIPLIER, DEFAULT_SEGMENTATION_PARALLEL_MIN_PIXELS,
    };
    use crate::pipeline::preprocess::CoordinateTransform;
    use ndarray::ArrayD;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::domain::prelude::{
        AnomalyMap, BoundingBox, Classification, Detection, Keypoint, KeypointDetection,
        SegmentationMask,
    };
    use rayon::prelude::*;
    use std::{cmp::Ordering, sync::Arc};

    // ── Raw inference output ───────────────────────────────────────

    /// Raw inference output from ONNX Runtime.
    #[derive(Debug)]
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
            labels: &[Arc<str>],
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
        pub nms_variant: NmsVariant,
        pub detection_parallel_threshold: usize,
        pub nms_prescreen_multiplier: usize,
    }

    impl Default for YoloV8PostProcessor {
        fn default() -> Self {
            Self {
                confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
                nms_iou_threshold: DEFAULT_NMS_IOU_THRESHOLD,
                max_detections: DEFAULT_MAX_DETECTIONS,
                nms_variant: NmsVariant::Classic,
                detection_parallel_threshold: DEFAULT_DETECTION_PARALLEL_THRESHOLD,
                nms_prescreen_multiplier: DEFAULT_NMS_PRESCREEN_MULTIPLIER,
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
            labels: &[Arc<str>],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = first_output_tensor(output, self.name())?;
            let shape = tensor.shape();
            if shape.len() != 3 {
                return Err(AiEngineError::PostprocessError(format!(
                    "expected 3D tensor, got {}D",
                    shape.len()
                )));
            }
            if shape[1] < 5 {
                return Err(AiEngineError::PostprocessError(format!(
                    "{} expects at least 5 features (4 bbox + >=1 class), got {}",
                    self.name(),
                    shape[1]
                )));
            }
            let num_features = shape[1];
            let num_preds = shape[2];
            if num_preds == 0 {
                return Ok(PostprocessOutput::default());
            }
            let num_classes = num_features - 4;

            let mut raw: Vec<(Detection, f32)> = Vec::with_capacity(512);
            if let Some(data) = tensor.as_slice_memory_order() {
                if num_preds >= self.detection_parallel_threshold {
                    raw = (0..num_preds)
                        .into_par_iter()
                        .filter_map(|i| {
                            let cx = data[i];
                            let cy = data[num_preds + i];
                            let w = data[2 * num_preds + i];
                            let h = data[3 * num_preds + i];

                            let mut best_class = 0usize;
                            let mut best_score = 0.0f32;
                            for c in 0..num_classes {
                                let score = data[(4 + c) * num_preds + i];
                                if score > best_score {
                                    best_score = score;
                                    best_class = c;
                                }
                            }

                            if best_score < self.confidence_threshold {
                                return None;
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
                            Some((
                                Detection {
                                    bbox,
                                    class: resolve_class_label(labels, best_class),
                                    class_id: best_class as u32,
                                    confidence: best_score,
                                    track_id: None,
                                },
                                best_score,
                            ))
                        })
                        .collect();
                } else {
                    for i in 0..num_preds {
                        let cx = data[i];
                        let cy = data[num_preds + i];
                        let w = data[2 * num_preds + i];
                        let h = data[3 * num_preds + i];

                        let mut best_class = 0usize;
                        let mut best_score = 0.0f32;
                        for c in 0..num_classes {
                            let score = data[(4 + c) * num_preds + i];
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

                        raw.push((
                            Detection {
                                bbox,
                                class: resolve_class_label(labels, best_class),
                                class_id: best_class as u32,
                                confidence: best_score,
                                track_id: None,
                            },
                            best_score,
                        ));
                    }
                }
            } else {
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

                    raw.push((
                        Detection {
                            bbox,
                            class: resolve_class_label(labels, best_class),
                            class_id: best_class as u32,
                            confidence: best_score,
                            track_id: None,
                        },
                        best_score,
                    ));
                }
            }

            let detections = nms_per_class_with_variant(
                raw,
                self.nms_iou_threshold,
                self.max_detections,
                self.nms_variant,
                self.nms_prescreen_multiplier,
            );
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
        pub nms_variant: NmsVariant,
        pub detection_parallel_threshold: usize,
        pub nms_prescreen_multiplier: usize,
    }

    impl Default for YoloV5PostProcessor {
        fn default() -> Self {
            Self {
                confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
                nms_iou_threshold: DEFAULT_NMS_IOU_THRESHOLD,
                max_detections: DEFAULT_MAX_DETECTIONS,
                nms_variant: NmsVariant::Classic,
                detection_parallel_threshold: DEFAULT_DETECTION_PARALLEL_THRESHOLD,
                nms_prescreen_multiplier: DEFAULT_NMS_PRESCREEN_MULTIPLIER,
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
            labels: &[Arc<str>],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = first_output_tensor(output, self.name())?;
            let shape = tensor.shape();
            if shape.len() != 3 {
                return Err(AiEngineError::PostprocessError(format!(
                    "expected 3D tensor, got {}D",
                    shape.len()
                )));
            }
            if shape[2] < 6 {
                return Err(AiEngineError::PostprocessError(format!(
                    "{} expects features >= 6 (5 base + >=1 class), got {}",
                    self.name(),
                    shape[2]
                )));
            }
            let num_preds = shape[1];
            if num_preds == 0 {
                return Ok(PostprocessOutput::default());
            }
            let num_classes = shape[2] - 5;

            let mut raw: Vec<(Detection, f32)> = Vec::with_capacity(512);
            if let Some(data) = tensor.as_slice_memory_order() {
                let feat_stride = shape[2];
                if num_preds >= self.detection_parallel_threshold {
                    let chunk_size = DEFAULT_DETECTION_PARALLEL_CHUNK_SIZE;
                    let chunk_count = num_preds.div_ceil(chunk_size);
                    raw = (0..chunk_count)
                        .into_par_iter()
                        .map(|chunk_idx| {
                            let start = chunk_idx * chunk_size;
                            let end = ((chunk_idx + 1) * chunk_size).min(num_preds);
                            let mut local = Vec::with_capacity((end - start) / 4 + 8);
                            for i in start..end {
                                let row = i * feat_stride;
                                let row_slice = &data[row..row + feat_stride];
                                let objectness = row_slice[4];
                                if objectness < self.confidence_threshold {
                                    continue;
                                }

                                let mut best_class = 0usize;
                                let mut best_score = 0.0f32;
                                for (c, &class_logit) in row_slice[5..].iter().enumerate() {
                                    let score = class_logit * objectness;
                                    if score > best_score {
                                        best_score = score;
                                        best_class = c;
                                    }
                                }

                                if best_score < self.confidence_threshold {
                                    continue;
                                }

                                let cx = row_slice[0];
                                let cy = row_slice[1];
                                let w = row_slice[2];
                                let h = row_slice[3];
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
                                local.push((
                                    Detection {
                                        bbox,
                                        class: resolve_class_label(labels, best_class),
                                        class_id: best_class as u32,
                                        confidence: best_score,
                                        track_id: None,
                                    },
                                    best_score,
                                ));
                            }
                            local
                        })
                        .reduce(Vec::new, |mut left, mut right| {
                            left.append(&mut right);
                            left
                        });
                } else {
                    for i in 0..num_preds {
                        let row = i * feat_stride;
                        let row_slice = &data[row..row + feat_stride];
                        let objectness = row_slice[4];
                        if objectness < self.confidence_threshold {
                            continue;
                        }

                        let mut best_class = 0usize;
                        let mut best_score = 0.0f32;
                        for (c, &class_logit) in row_slice[5..].iter().enumerate() {
                            let score = class_logit * objectness;
                            if score > best_score {
                                best_score = score;
                                best_class = c;
                            }
                        }

                        if best_score < self.confidence_threshold {
                            continue;
                        }

                        let cx = row_slice[0];
                        let cy = row_slice[1];
                        let w = row_slice[2];
                        let h = row_slice[3];
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

                        raw.push((
                            Detection {
                                bbox,
                                class: resolve_class_label(labels, best_class),
                                class_id: best_class as u32,
                                confidence: best_score,
                                track_id: None,
                            },
                            best_score,
                        ));
                    }
                }
            } else {
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

                    raw.push((
                        Detection {
                            bbox,
                            class: resolve_class_label(labels, best_class),
                            class_id: best_class as u32,
                            confidence: best_score,
                            track_id: None,
                        },
                        best_score,
                    ));
                }
            }

            let detections = nms_per_class_with_variant(
                raw,
                self.nms_iou_threshold,
                self.max_detections,
                self.nms_variant,
                self.nms_prescreen_multiplier,
            );
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
        pub small_class_fast_path: usize,
    }

    impl Default for ClassificationPostProcessor {
        fn default() -> Self {
            Self {
                top_k: DEFAULT_CLASSIFICATION_TOP_K,
                apply_softmax: DEFAULT_CLASSIFICATION_APPLY_SOFTMAX,
                min_confidence: 0.01,
                small_class_fast_path: DEFAULT_CLASSIFICATION_SMALL_CLASS_FAST_PATH,
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
            labels: &[Arc<str>],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = first_output_tensor(output, self.name())?;
            let shape = tensor.shape();
            if shape.len() != 2 {
                return Err(AiEngineError::PostprocessError(format!(
                    "{} expects 2D tensor [1, C], got {}D",
                    self.name(),
                    shape.len()
                )));
            }
            if shape[0] == 0 {
                return Err(AiEngineError::PostprocessError(format!(
                    "{} received empty batch dimension",
                    self.name()
                )));
            }
            let num_classes = tensor.shape()[1];
            if num_classes == 0 || self.top_k == 0 {
                return Ok(PostprocessOutput {
                    classifications: vec![Classification { top_k: Vec::new() }],
                    ..Default::default()
                });
            }

            let mut scores: Vec<f32> = if let Some(data) = tensor.as_slice_memory_order() {
                data.iter().copied().take(num_classes).collect()
            } else {
                (0..num_classes).map(|i| tensor[[0, i]]).collect()
            };

            if self.apply_softmax {
                let max_val = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let exp_sum: f32 = scores.iter().map(|&s| (s - max_val).exp()).sum();
                for s in &mut scores {
                    *s = (*s - max_val).exp() / exp_sum;
                }
            }

            let mut indexed: Vec<(usize, f32)> = scores.into_iter().enumerate().collect();
            let top_k_target = self.top_k.min(indexed.len());
            if indexed.len() <= self.small_class_fast_path {
                indexed.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
                indexed.truncate(top_k_target);
            } else if top_k_target < indexed.len() {
                let kth = top_k_target - 1;
                indexed.select_nth_unstable_by(kth, |a, b| {
                    b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal)
                });
                indexed.truncate(top_k_target);
            }
            indexed.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

            let top_k: Vec<(Arc<str>, f32)> = indexed
                .into_iter()
                .filter(|(_, score)| *score >= self.min_confidence)
                .map(|(idx, score)| {
                    let label = resolve_class_label(labels, idx);
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
            _labels: &[Arc<str>],
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
    pub struct SegmentationPostProcessor {
        pub parallel_min_pixels: usize,
    }

    impl Default for SegmentationPostProcessor {
        fn default() -> Self {
            Self {
                parallel_min_pixels: DEFAULT_SEGMENTATION_PARALLEL_MIN_PIXELS,
            }
        }
    }

    impl PostProcessor for SegmentationPostProcessor {
        fn name(&self) -> &str {
            "segmentation"
        }

        fn process(
            &self,
            output: &RawInferenceOutput,
            _coord_transform: &CoordinateTransform,
            labels: &[Arc<str>],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = first_output_tensor(output, self.name())?;
            let shape = tensor.shape();
            if shape.len() != 4 {
                return Err(AiEngineError::PostprocessError(format!(
                    "segmentation expects 4D tensor [1,C,H,W], got {}D",
                    shape.len()
                )));
            }
            if shape[0] == 0 {
                return Err(AiEngineError::PostprocessError(
                    "segmentation expects non-empty batch dimension".into(),
                ));
            }
            let num_classes = shape[1];
            if num_classes == 0 {
                return Err(AiEngineError::PostprocessError(
                    "segmentation expects at least one class channel".into(),
                ));
            }
            let h = shape[2];
            let w = shape[3];

            let mut mask = Vec::with_capacity(h * w);
            if let Some(data) = tensor.as_slice_memory_order() {
                let hw = h * w;
                mask.resize(hw, 0u8);
                if hw >= self.parallel_min_pixels {
                    mask.par_iter_mut().enumerate().for_each(|(p, out)| {
                        let mut best_class = 0u8;
                        let mut best_score = f32::NEG_INFINITY;
                        for c in 0..num_classes {
                            let score = data[c * hw + p];
                            if score > best_score {
                                best_score = score;
                                best_class = c as u8;
                            }
                        }
                        *out = best_class;
                    });
                } else {
                    for (p, out) in mask.iter_mut().enumerate() {
                        let mut best_class = 0u8;
                        let mut best_score = f32::NEG_INFINITY;
                        for c in 0..num_classes {
                            let score = data[c * hw + p];
                            if score > best_score {
                                best_score = score;
                                best_class = c as u8;
                            }
                        }
                        *out = best_class;
                    }
                }
            } else {
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
            }

            Ok(PostprocessOutput {
                segmentation_masks: vec![SegmentationMask {
                    mask,
                    width: w as u32,
                    height: h as u32,
                    labels: labels.to_vec(),
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
                confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
                nms_iou_threshold: DEFAULT_NMS_IOU_THRESHOLD,
                max_detections: DEFAULT_KEYPOINT_MAX_DETECTIONS,
                num_keypoints: DEFAULT_KEYPOINT_COUNT,
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
            labels: &[Arc<str>],
        ) -> Result<PostprocessOutput, AiEngineError> {
            let tensor = first_output_tensor(output, self.name())?;
            let shape = tensor.shape();

            if shape.len() != 3 {
                return Err(AiEngineError::PostprocessError(format!(
                    "yolov8_pose expects 3D tensor [1, F, N], got {}D",
                    shape.len()
                )));
            }

            let num_features = shape[1];
            let num_preds = shape[2];
            if num_preds == 0 {
                return Ok(PostprocessOutput::default());
            }
            let expected_features = 5 + self.num_keypoints * 3;

            if num_features < expected_features {
                return Err(AiEngineError::PostprocessError(format!(
                    "yolov8_pose expects >= {} features (5 + {}×3), got {}",
                    expected_features, self.num_keypoints, num_features
                )));
            }

            let class_label = labels
                .first()
                .map(Arc::clone)
                .unwrap_or(Arc::from("person"));

            let iw = coord_transform.input_width as f32;
            let ih = coord_transform.input_height as f32;

            let mut raw: Vec<(KeypointDetection, f32)> = Vec::with_capacity(256);
            if let Some(data) = tensor.as_slice_memory_order() {
                for i in 0..num_preds {
                    let cx = data[i];
                    let cy = data[num_preds + i];
                    let w = data[2 * num_preds + i];
                    let h = data[3 * num_preds + i];
                    let score = data[4 * num_preds + i];

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
                        let base = (5 + k * 3) * num_preds + i;
                        let kx = data[base];
                        let ky = data[base + num_preds];
                        let kv = data[base + 2 * num_preds];

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
            } else {
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
            }

            let detections =
                nms_keypoint_detections(raw, self.nms_iou_threshold, self.max_detections);

            Ok(PostprocessOutput {
                keypoint_detections: detections,
                ..Default::default()
            })
        }
    }

    /// Greedy NMS for keypoint detections (class-agnostic since typically single-class).
    fn nms_keypoint_detections(
        mut raw: Vec<(KeypointDetection, f32)>,
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<KeypointDetection> {
        raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

        let mut suppressed = vec![false; raw.len()];
        let mut keep_indices = Vec::with_capacity(max_detections.min(raw.len()));

        for i in 0..raw.len() {
            if suppressed[i] || keep_indices.len() >= max_detections {
                continue;
            }
            keep_indices.push(i);

            for j in (i + 1)..raw.len() {
                if suppressed[j] {
                    continue;
                }
                if raw[i].0.bbox.iou(&raw[j].0.bbox) > iou_threshold {
                    suppressed[j] = true;
                }
            }
        }

        let mut moved: Vec<Option<KeypointDetection>> = raw
            .into_iter()
            .map(|(detection, _)| Some(detection))
            .collect();
        let mut keep = Vec::with_capacity(keep_indices.len());
        for idx in keep_indices {
            if let Some(detection) = moved[idx].take() {
                keep.push(detection);
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
                anomaly_threshold: DEFAULT_ANOMALY_THRESHOLD,
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
            _labels: &[Arc<str>],
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

    /// Build a class-id indexed label cache to avoid string lookups/allocation
    /// in hot detection loops.
    fn first_output_tensor<'a>(
        output: &'a RawInferenceOutput,
        processor_name: &str,
    ) -> Result<&'a ArrayD<f32>, AiEngineError> {
        output
            .tensors
            .first()
            .map(|(_, tensor)| tensor)
            .ok_or_else(|| {
                AiEngineError::PostprocessError(format!("{processor_name}: no output tensors"))
            })
    }

    /// Resolve class label by index from precompiled Arc cache.
    #[inline]
    fn resolve_class_label(labels: &[Arc<str>], class_id: usize) -> Arc<str> {
        labels
            .get(class_id)
            .map(Arc::clone)
            .unwrap_or(Arc::from(format!("class_{class_id}")))
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
        raw: Vec<(Detection, f32)>,
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        nms_per_class_with_variant(
            raw,
            iou_threshold,
            max_detections,
            NmsVariant::Classic,
            DEFAULT_NMS_PRESCREEN_MULTIPLIER,
        )
    }

    /// NMS per class with configurable algorithm variant.
    pub fn nms_per_class_with_variant(
        mut raw: Vec<(Detection, f32)>,
        iou_threshold: f32,
        max_detections: usize,
        variant: NmsVariant,
        nms_prescreen_multiplier: usize,
    ) -> Vec<Detection> {
        prescreen_candidates_by_score(&mut raw, max_detections, nms_prescreen_multiplier);
        raw.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

        match variant {
            NmsVariant::Classic => nms_classic(raw, iou_threshold, max_detections),
            NmsVariant::Soft { sigma } => nms_soft(&mut raw, sigma, iou_threshold, max_detections),
            NmsVariant::DIoU => nms_diou(raw, iou_threshold, max_detections),
        }
    }

    /// Classic greedy NMS: hard-suppress overlapping detections.
    fn nms_classic(
        raw: Vec<(Detection, f32)>,
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        let mut suppressed = vec![false; raw.len()];
        let mut keep_indices = Vec::with_capacity(max_detections.min(raw.len()));

        for i in 0..raw.len() {
            if suppressed[i] || keep_indices.len() >= max_detections {
                continue;
            }
            keep_indices.push(i);

            for j in (i + 1)..raw.len() {
                if suppressed[j] || raw[j].0.class_id != raw[i].0.class_id {
                    continue;
                }
                if raw[i].0.bbox.iou(&raw[j].0.bbox) > iou_threshold {
                    suppressed[j] = true;
                }
            }
        }

        let mut moved: Vec<Option<Detection>> = raw
            .into_iter()
            .map(|(detection, _)| Some(detection))
            .collect();
        let mut keep = Vec::with_capacity(keep_indices.len());
        for idx in keep_indices {
            if let Some(detection) = moved[idx].take() {
                keep.push(detection);
            }
        }
        keep
    }

    /// Reduce NMS input size before sort/suppression to contain O(n²) cost.
    ///
    /// This keeps the highest-confidence candidates globally as a fast pre-screen.
    fn prescreen_candidates_by_score(
        raw: &mut Vec<(Detection, f32)>,
        max_detections: usize,
        nms_prescreen_multiplier: usize,
    ) {
        let multiplier = nms_prescreen_multiplier.max(1);
        let keep_limit = max_detections
            .saturating_mul(multiplier)
            .max(max_detections);
        if raw.len() <= keep_limit {
            return;
        }
        let kth = keep_limit - 1;
        raw.select_nth_unstable_by(kth, |a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        raw.truncate(keep_limit);
    }

    /// Soft-NMS: instead of hard suppression, decay confidence using
    /// a Gaussian penalty: `score *= exp(-iou² / sigma)`.
    ///
    /// This preserves partially overlapping detections that classic NMS
    /// would discard, improving recall in crowded scenes.
    fn nms_soft(
        raw: &mut Vec<(Detection, f32)>,
        sigma: f32,
        score_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        let mut keep_mask = vec![false; raw.len()];
        let mut keep_count = 0usize;

        for i in 0..raw.len() {
            if keep_count >= max_detections {
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
            keep_mask[i] = true;
            keep_count += 1;

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

        let mut keep = Vec::with_capacity(keep_count);
        let moved = std::mem::take(raw);
        for (idx, (detection, _score)) in moved.into_iter().enumerate() {
            if keep_mask[idx] {
                keep.push(detection);
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
        raw: Vec<(Detection, f32)>,
        iou_threshold: f32,
        max_detections: usize,
    ) -> Vec<Detection> {
        let mut suppressed = vec![false; raw.len()];
        let mut keep_indices = Vec::with_capacity(max_detections.min(raw.len()));

        for i in 0..raw.len() {
            if suppressed[i] || keep_indices.len() >= max_detections {
                continue;
            }
            keep_indices.push(i);

            for j in (i + 1)..raw.len() {
                if suppressed[j] || raw[j].0.class_id != raw[i].0.class_id {
                    continue;
                }
                if diou(&raw[i].0.bbox, &raw[j].0.bbox) > iou_threshold {
                    suppressed[j] = true;
                }
            }
        }

        let mut moved: Vec<Option<Detection>> = raw
            .into_iter()
            .map(|(detection, _)| Some(detection))
            .collect();
        let mut keep = Vec::with_capacity(keep_indices.len());
        for idx in keep_indices {
            if let Some(detection) = moved[idx].take() {
                keep.push(detection);
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::test_utils::*;
        use approx::assert_abs_diff_eq;
        use ndarray::ArrayD;
        use std::sync::Arc;

        fn labels(n: usize) -> Vec<Arc<str>> {
            (0..n)
                .map(|i| Arc::<str>::from(format!("class_{i}")))
                .collect()
        }

        // ── YOLOv8 detection tests ──────────────────────────────────

        #[test]
        fn yolov8_single_detection_produces_correct_bbox() {
            // Place one high-confidence detection at the center of a 640×640 input.
            // cx=320, cy=320, w=100, h=100 → expected normalised bbox ≈ [0.42, 0.42, 0.58, 0.58].
            let output = make_yolov8_output(&[(320.0, 320.0, 100.0, 100.0, vec![0.95])], 1);
            let transform = identity_transform(640, 640);
            let proc = YoloV8PostProcessor::default();

            let result = proc
                .process(&output, &transform, &labels(1))
                .expect("process should succeed");

            assert_eq!(result.detections.len(), 1);
            let det = &result.detections[0];
            assert_abs_diff_eq!(det.bbox.x_min, 0.421875, epsilon = 0.01);
            assert_abs_diff_eq!(det.bbox.y_min, 0.421875, epsilon = 0.01);
            assert_abs_diff_eq!(det.bbox.x_max, 0.578125, epsilon = 0.01);
            assert_abs_diff_eq!(det.bbox.y_max, 0.578125, epsilon = 0.01);
            assert_abs_diff_eq!(det.confidence, 0.95, epsilon = 1e-5);
        }

        #[test]
        fn yolov8_nms_suppresses_overlapping() {
            // Two nearly identical detections for the same class — NMS should keep only 1.
            let output = make_yolov8_output(
                &[
                    (320.0, 320.0, 100.0, 100.0, vec![0.9]),
                    (325.0, 325.0, 100.0, 100.0, vec![0.85]),
                ],
                1,
            );
            let transform = identity_transform(640, 640);
            let proc = YoloV8PostProcessor {
                nms_iou_threshold: 0.5,
                ..YoloV8PostProcessor::default()
            };

            let result = proc
                .process(&output, &transform, &labels(1))
                .expect("process should succeed");

            assert_eq!(
                result.detections.len(),
                1,
                "NMS should suppress the lower-confidence duplicate"
            );
            assert_abs_diff_eq!(result.detections[0].confidence, 0.9, epsilon = 1e-5);
        }

        #[test]
        fn yolov8_confidence_threshold_filters() {
            // One detection above threshold, one below — only 1 should survive.
            let output = make_yolov8_output(
                &[
                    (320.0, 320.0, 100.0, 100.0, vec![0.8]),
                    (100.0, 100.0, 50.0, 50.0, vec![0.1]),
                ],
                1,
            );
            let transform = identity_transform(640, 640);
            let proc = YoloV8PostProcessor {
                confidence_threshold: 0.5,
                ..YoloV8PostProcessor::default()
            };

            let result = proc
                .process(&output, &transform, &labels(1))
                .expect("process should succeed");

            assert_eq!(
                result.detections.len(),
                1,
                "sub-threshold detection must be discarded"
            );
            assert_abs_diff_eq!(result.detections[0].confidence, 0.8, epsilon = 1e-5);
        }

        #[test]
        fn yolov8_max_detections_limit() {
            // 10 well-separated detections with max_detections=3 → only 3 kept.
            let dets: Vec<(f32, f32, f32, f32, Vec<f32>)> = (0..10)
                .map(|i| {
                    let cx = 50.0 + i as f32 * 60.0;
                    let score = 0.9 - i as f32 * 0.02;
                    (cx, 320.0, 40.0, 40.0, vec![score])
                })
                .collect();
            let output = make_yolov8_output(&dets, 1);
            let transform = identity_transform(640, 640);
            let proc = YoloV8PostProcessor {
                max_detections: 3,
                ..YoloV8PostProcessor::default()
            };

            let result = proc
                .process(&output, &transform, &labels(1))
                .expect("process should succeed");

            assert!(
                result.detections.len() <= 3,
                "max_detections=3 must cap output, got {}",
                result.detections.len()
            );
        }

        // ── YOLOv5 detection tests ──────────────────────────────────

        #[test]
        fn yolov5_single_detection_correct() {
            // Single YOLOv5-format prediction: objectness=0.9, class_score=0.85.
            // Effective score = 0.9 * 0.85 = 0.765, which exceeds default threshold.
            let output = make_yolov5_output(&[(320.0, 320.0, 100.0, 100.0, 0.9, vec![0.85])], 1);
            let transform = identity_transform(640, 640);
            let proc = YoloV5PostProcessor::default();

            let result = proc
                .process(&output, &transform, &labels(1))
                .expect("process should succeed");

            assert_eq!(result.detections.len(), 1);
            let det = &result.detections[0];
            assert_abs_diff_eq!(det.confidence, 0.9 * 0.85, epsilon = 1e-5);
            assert!(det.bbox.x_min < det.bbox.x_max);
            assert!(det.bbox.y_min < det.bbox.y_max);
        }

        #[test]
        fn yolov5_objectness_gates_class_scores() {
            // Low objectness (0.1) × high class score (0.95) = 0.095 < threshold → filtered.
            let output = make_yolov5_output(&[(320.0, 320.0, 100.0, 100.0, 0.1, vec![0.95])], 1);
            let transform = identity_transform(640, 640);
            let proc = YoloV5PostProcessor {
                confidence_threshold: 0.5,
                ..YoloV5PostProcessor::default()
            };

            let result = proc
                .process(&output, &transform, &labels(1))
                .expect("process should succeed");

            assert!(
                result.detections.is_empty(),
                "low objectness should gate out high class scores"
            );
        }

        // ── Classification tests ────────────────────────────────────

        #[test]
        fn classification_softmax_sums_to_one() {
            // Raw logits; after softmax the probabilities must sum to 1.0.
            let raw_logits: Vec<f32> = vec![2.0, 1.0, 0.5, -1.0, 3.0];
            let output = make_classification_output(&raw_logits);
            let transform = identity_transform(640, 640);
            let proc = ClassificationPostProcessor {
                top_k: 5,
                apply_softmax: true,
                min_confidence: 0.0,
                ..ClassificationPostProcessor::default()
            };

            let result = proc
                .process(&output, &transform, &labels(5))
                .expect("process should succeed");

            assert_eq!(result.classifications.len(), 1);
            let sum: f32 = result.classifications[0].top_k.iter().map(|(_, s)| s).sum();
            assert_abs_diff_eq!(sum, 1.0, epsilon = 1e-4);
        }

        #[test]
        fn classification_topk_returns_correct_count() {
            // 1000-class model with top_k=5 → exactly 5 results.
            let scores: Vec<f32> = (0..1000).map(|i| (i as f32) / 1000.0).collect();
            let output = make_classification_output(&scores);
            let transform = identity_transform(640, 640);
            let proc = ClassificationPostProcessor {
                top_k: 5,
                apply_softmax: false,
                min_confidence: 0.0,
                ..ClassificationPostProcessor::default()
            };

            let result = proc
                .process(&output, &transform, &labels(1000))
                .expect("process should succeed");

            assert_eq!(result.classifications.len(), 1);
            assert_eq!(
                result.classifications[0].top_k.len(),
                5,
                "top_k=5 must return exactly 5 entries"
            );
        }

        #[test]
        fn classification_single_class() {
            // Edge case: 1-class model → single classification entry.
            let output = make_classification_output(&[0.99]);
            let transform = identity_transform(640, 640);
            let proc = ClassificationPostProcessor {
                top_k: 5,
                apply_softmax: false,
                min_confidence: 0.0,
                ..ClassificationPostProcessor::default()
            };

            let result = proc
                .process(&output, &transform, &labels(1))
                .expect("process should succeed");

            assert_eq!(result.classifications.len(), 1);
            assert_eq!(result.classifications[0].top_k.len(), 1);
            assert_abs_diff_eq!(result.classifications[0].top_k[0].1, 0.99, epsilon = 1e-5);
        }

        // ── Segmentation tests ──────────────────────────────────────

        #[test]
        fn segmentation_argmax_correctness() {
            // 3-class 2×2 segmentation tensor. Manually craft per-pixel winners.
            // Pixel layout (row-major): [0]=class2, [1]=class0, [2]=class1, [3]=class2
            #[rustfmt::skip]
            let data = vec![
                // class 0 logits (C=0, H=2, W=2)
                0.1, 0.9, 0.2, 0.1,
                // class 1 logits (C=1, H=2, W=2)
                0.2, 0.1, 0.8, 0.3,
                // class 2 logits (C=2, H=2, W=2)
                0.9, 0.1, 0.1, 0.7,
            ];
            let output = make_segmentation_output(data, 3, 2, 2);
            let transform = identity_transform(640, 640);
            let proc = SegmentationPostProcessor::default();

            let result = proc
                .process(&output, &transform, &labels(3))
                .expect("process should succeed");

            assert_eq!(result.segmentation_masks.len(), 1);
            let mask = &result.segmentation_masks[0];
            assert_eq!(mask.width, 2);
            assert_eq!(mask.height, 2);
            assert_eq!(mask.mask, vec![2, 0, 1, 2]);
        }

        // ── Anomaly detection tests ─────────────────────────────────

        #[test]
        fn anomaly_score_above_threshold_is_anomalous() {
            // score=0.8 with threshold=0.5 → is_anomalous=true.
            let tensor = ArrayD::from_shape_vec(vec![1, 1], vec![0.8]).expect("valid shape");
            let output = RawInferenceOutput {
                tensors: vec![("anomaly_score".into(), tensor)],
            };
            let transform = identity_transform(640, 640);
            let proc = AnomalyPostProcessor {
                anomaly_threshold: 0.5,
            };

            let result = proc
                .process(&output, &transform, &[])
                .expect("process should succeed");

            assert_eq!(result.anomaly_maps.len(), 1);
            assert!(result.anomaly_maps[0].is_anomalous);
            assert_abs_diff_eq!(result.anomaly_maps[0].score, 0.8, epsilon = 1e-5);
        }

        #[test]
        fn anomaly_score_below_threshold_not_anomalous() {
            // score=0.3 with threshold=0.5 → is_anomalous=false.
            let tensor = ArrayD::from_shape_vec(vec![1, 1], vec![0.3]).expect("valid shape");
            let output = RawInferenceOutput {
                tensors: vec![("anomaly_score".into(), tensor)],
            };
            let transform = identity_transform(640, 640);
            let proc = AnomalyPostProcessor {
                anomaly_threshold: 0.5,
            };

            let result = proc
                .process(&output, &transform, &[])
                .expect("process should succeed");

            assert_eq!(result.anomaly_maps.len(), 1);
            assert!(!result.anomaly_maps[0].is_anomalous);
            assert_abs_diff_eq!(result.anomaly_maps[0].score, 0.3, epsilon = 1e-5);
        }

        // ── NMS variant comparison ──────────────────────────────────

        #[test]
        fn nms_classic_vs_soft_vs_diou_behavior() {
            // Three highly-overlapping same-class detections. Classic NMS should
            // suppress more aggressively than Soft-NMS (Gaussian decay preserves
            // partially overlapping boxes).
            let make_raw = || -> Vec<(Detection, f32)> {
                vec![
                    (
                        Detection {
                            bbox: BoundingBox {
                                x_min: 0.0,
                                y_min: 0.0,
                                x_max: 0.5,
                                y_max: 0.5,
                            },
                            class: Arc::from("a"),
                            class_id: 0,
                            confidence: 0.9,
                            track_id: None,
                        },
                        0.9,
                    ),
                    (
                        Detection {
                            bbox: BoundingBox {
                                x_min: 0.02,
                                y_min: 0.02,
                                x_max: 0.52,
                                y_max: 0.52,
                            },
                            class: Arc::from("a"),
                            class_id: 0,
                            confidence: 0.85,
                            track_id: None,
                        },
                        0.85,
                    ),
                    (
                        Detection {
                            bbox: BoundingBox {
                                x_min: 0.04,
                                y_min: 0.04,
                                x_max: 0.54,
                                y_max: 0.54,
                            },
                            class: Arc::from("a"),
                            class_id: 0,
                            confidence: 0.8,
                            track_id: None,
                        },
                        0.8,
                    ),
                ]
            };

            let classic = nms_per_class_with_variant(make_raw(), 0.5, 10, NmsVariant::Classic, 8);
            let soft =
                nms_per_class_with_variant(make_raw(), 0.5, 10, NmsVariant::Soft { sigma: 0.5 }, 8);
            let diou = nms_per_class_with_variant(make_raw(), 0.5, 10, NmsVariant::DIoU, 8);

            // Classic should be the most aggressive (fewest survivors).
            assert!(
                classic.len() <= soft.len(),
                "Classic NMS ({}) should suppress at least as many as Soft-NMS ({})",
                classic.len(),
                soft.len()
            );
            // DIoU considers center distance — for boxes with similar centers the
            // result should be comparable to Classic.
            assert!(
                diou.len() <= soft.len(),
                "DIoU-NMS ({}) should suppress at least as aggressively as Soft-NMS ({})",
                diou.len(),
                soft.len()
            );
        }

        // ── Edge cases ──────────────────────────────────────────────

        #[test]
        fn postprocess_empty_tensor_returns_default() {
            // YOLOv8 tensor with 0 predictions → empty PostprocessOutput.
            let output = make_yolov8_output(&[], 3);
            let transform = identity_transform(640, 640);
            let proc = YoloV8PostProcessor::default();

            let result = proc
                .process(&output, &transform, &labels(3))
                .expect("process should succeed");

            assert!(result.detections.is_empty());
            assert!(result.classifications.is_empty());
            assert!(result.segmentation_masks.is_empty());
            assert!(result.anomaly_maps.is_empty());
        }

        #[test]
        fn postprocess_invalid_shape_returns_error() {
            // Feed a 2D tensor to YOLOv8 — must produce PostprocessError.
            let tensor = ArrayD::from_shape_vec(vec![1, 8], vec![0.0; 8]).expect("valid shape");
            let output = RawInferenceOutput {
                tensors: vec![("output0".into(), tensor)],
            };
            let transform = identity_transform(640, 640);
            let proc = YoloV8PostProcessor::default();

            let result = proc.process(&output, &transform, &labels(3));

            assert!(result.is_err(), "2D tensor should trigger PostprocessError");
            let err = result.unwrap_err();
            assert!(
                matches!(err, AiEngineError::PostprocessError(_)),
                "expected PostprocessError, got: {err:?}"
            );
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
