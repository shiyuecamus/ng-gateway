#![cfg(feature = "engine")]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ndarray::{Array, ArrayD, IxDyn};
use ng_gateway_ai::{
    api::AiEngineError,
    pipeline::{
        postprocess::{
            nms_per_class_with_variant, ClassificationPostProcessor, NmsVariant, PostProcessor,
            PostprocessOutput, RawInferenceOutput, SegmentationPostProcessor, YoloV5PostProcessor,
            YoloV8PostProcessor,
        },
        preprocess::CoordinateTransform,
    },
};
use ng_gateway_models::domain::prelude::{BoundingBox, Detection};
use rayon::prelude::*;
use std::{hint::black_box, sync::Arc};

#[derive(Clone)]
struct LegacyYoloV5PostProcessor {
    confidence_threshold: f32,
    nms_iou_threshold: f32,
    max_detections: usize,
    nms_variant: NmsVariant,
    detection_parallel_threshold: usize,
    nms_prescreen_multiplier: usize,
}

#[derive(Clone)]
struct ChunkedYoloV5PostProcessor {
    confidence_threshold: f32,
    nms_iou_threshold: f32,
    max_detections: usize,
    nms_variant: NmsVariant,
    detection_parallel_threshold: usize,
    nms_prescreen_multiplier: usize,
    chunk_size: usize,
}

impl Default for ChunkedYoloV5PostProcessor {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.5,
            nms_iou_threshold: 0.45,
            max_detections: 300,
            nms_variant: NmsVariant::Classic,
            detection_parallel_threshold: 8_192,
            nms_prescreen_multiplier: 8,
            chunk_size: 512,
        }
    }
}

impl Default for LegacyYoloV5PostProcessor {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.5,
            nms_iou_threshold: 0.45,
            max_detections: 300,
            nms_variant: NmsVariant::Classic,
            detection_parallel_threshold: 8_192,
            nms_prescreen_multiplier: 8,
        }
    }
}

impl PostProcessor for LegacyYoloV5PostProcessor {
    fn name(&self) -> &str {
        "legacy_yolov5_detection"
    }

    fn process(
        &self,
        output: &RawInferenceOutput,
        coord_transform: &CoordinateTransform,
        labels: &[String],
    ) -> Result<PostprocessOutput, AiEngineError> {
        let tensor = &output.tensors[0].1;
        let shape = tensor.shape();
        let num_preds = shape[1];
        let num_classes = shape[2] - 5;
        let label_cache: Vec<Arc<str>> = (0..num_classes)
            .map(|idx| {
                labels
                    .get(idx)
                    .map(|s| Arc::<str>::from(s.as_str()))
                    .unwrap_or(Arc::from(format!("class_{idx}")))
            })
            .collect();

        let mut raw: Vec<(Detection, f32)> = Vec::with_capacity(512);
        if let Some(data) = tensor.as_slice_memory_order() {
            let feat_stride = shape[2];
            if num_preds >= self.detection_parallel_threshold {
                raw = (0..num_preds)
                    .into_par_iter()
                    .filter_map(|i| {
                        let row = i * feat_stride;
                        let row_slice = &data[row..row + feat_stride];
                        let objectness = row_slice[4];
                        if objectness < self.confidence_threshold {
                            return None;
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
                            return None;
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
                        Some((
                            Detection {
                                bbox,
                                class: Arc::clone(&label_cache[best_class]),
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
                            class: Arc::clone(&label_cache[best_class]),
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
                        class: Arc::clone(&label_cache[best_class]),
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

impl PostProcessor for ChunkedYoloV5PostProcessor {
    fn name(&self) -> &str {
        "chunked_yolov5_detection"
    }

    fn process(
        &self,
        output: &RawInferenceOutput,
        coord_transform: &CoordinateTransform,
        labels: &[String],
    ) -> Result<PostprocessOutput, AiEngineError> {
        let tensor = &output.tensors[0].1;
        let shape = tensor.shape();
        let num_preds = shape[1];
        let num_classes = shape[2] - 5;
        let label_cache: Vec<Arc<str>> = (0..num_classes)
            .map(|idx| {
                labels
                    .get(idx)
                    .map(|s| Arc::<str>::from(s.as_str()))
                    .unwrap_or(Arc::from(format!("class_{idx}")))
            })
            .collect();

        let mut raw: Vec<(Detection, f32)> = Vec::with_capacity(512);
        if let Some(data) = tensor.as_slice_memory_order() {
            let feat_stride = shape[2];
            if num_preds >= self.detection_parallel_threshold {
                let chunk_size = self.chunk_size.max(64);
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
                                    class: Arc::clone(&label_cache[best_class]),
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
                            class: Arc::clone(&label_cache[best_class]),
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
                        class: Arc::clone(&label_cache[best_class]),
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

        Ok(ng_gateway_ai::pipeline::postprocess::PostprocessOutput {
            detections,
            ..Default::default()
        })
    }
}

fn make_coord_transform() -> CoordinateTransform {
    CoordinateTransform {
        scale_x: 1.0,
        scale_y: 1.0,
        pad_x: 0.0,
        pad_y: 0.0,
        orig_width: 640,
        orig_height: 640,
        input_width: 640,
        input_height: 640,
    }
}

fn make_yolov8_output(num_classes: usize, num_preds: usize) -> RawInferenceOutput {
    let features = 4 + num_classes;
    let mut data = vec![0.0f32; features * num_preds];
    for i in 0..num_preds {
        data[i] = 320.0;
        data[num_preds + i] = 320.0;
        data[2 * num_preds + i] = 128.0;
        data[3 * num_preds + i] = 128.0;
        for c in 0..num_classes {
            let score = ((i + c * 17) % 100) as f32 / 100.0;
            data[(4 + c) * num_preds + i] = score;
        }
    }
    let tensor = Array::from_shape_vec(IxDyn(&[1, features, num_preds]), data)
        .expect("yolov8 tensor shape should match");
    RawInferenceOutput {
        tensors: vec![("output0".to_string(), tensor.into_dyn())],
    }
}

fn make_yolov5_output(num_classes: usize, num_preds: usize) -> RawInferenceOutput {
    let features = 5 + num_classes;
    let mut data = vec![0.0f32; num_preds * features];
    for i in 0..num_preds {
        let row = i * features;
        data[row] = 320.0;
        data[row + 1] = 320.0;
        data[row + 2] = 128.0;
        data[row + 3] = 128.0;
        data[row + 4] = 0.5 + ((i % 50) as f32 / 100.0);
        for c in 0..num_classes {
            data[row + 5 + c] = ((i + c * 17) % 100) as f32 / 100.0;
        }
    }
    let tensor = Array::from_shape_vec(IxDyn(&[1, num_preds, features]), data)
        .expect("yolov5 tensor shape should match");
    RawInferenceOutput {
        tensors: vec![("output0".to_string(), tensor.into_dyn())],
    }
}

fn make_segmentation_output(num_classes: usize, h: usize, w: usize) -> RawInferenceOutput {
    let hw = h * w;
    let mut data = vec![0.0f32; num_classes * hw];
    for c in 0..num_classes {
        for p in 0..hw {
            data[c * hw + p] = ((p + c * 13) % 101) as f32 / 101.0;
        }
    }
    let tensor = Array::from_shape_vec(IxDyn(&[1, num_classes, h, w]), data)
        .expect("segmentation tensor shape should match");
    RawInferenceOutput {
        tensors: vec![("output0".to_string(), tensor.into_dyn())],
    }
}

fn make_classification_output(num_classes: usize) -> RawInferenceOutput {
    let mut data = vec![0.0f32; num_classes];
    for (i, v) in data.iter_mut().enumerate() {
        *v = ((i * 31) % 997) as f32 / 997.0;
    }
    let tensor: ArrayD<f32> = Array::from_shape_vec(IxDyn(&[1, num_classes]), data)
        .expect("classification tensor shape should match")
        .into_dyn();
    RawInferenceOutput {
        tensors: vec![("output0".to_string(), tensor)],
    }
}

fn bench_yolov8_postprocess(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_yolov8");
    let labels: Vec<String> = (0..80).map(|i| format!("class_{i}")).collect();
    let coord = make_coord_transform();
    let processor = YoloV8PostProcessor::default();
    for &preds in &[8400usize, 25200usize] {
        let output = make_yolov8_output(80, preds);
        group.throughput(Throughput::Elements(preds as u64));
        group.bench_with_input(BenchmarkId::new("preds", preds), &output, |b, out| {
            b.iter(|| {
                let result = processor
                    .process(out, &coord, &labels)
                    .expect("yolov8 postprocess should succeed");
                black_box(result.detections.len());
            })
        });
    }
    group.finish();
}

fn bench_yolov8_threshold_matrix(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_yolov8_threshold_matrix");
    let labels: Vec<String> = (0..80).map(|i| format!("class_{i}")).collect();
    let coord = make_coord_transform();
    let preds = 25_200usize;
    let output = make_yolov8_output(80, preds);
    group.throughput(Throughput::Elements(preds as u64));

    let scenarios: [(&str, usize, usize); 4] = [
        ("baseline", 8_192, 8),
        ("force_parallel", 1, 8),
        ("aggressive_prescreen", 8_192, 4),
        ("conservative_prescreen", 8_192, 16),
    ];

    for (name, detection_parallel_threshold, nms_prescreen_multiplier) in scenarios {
        let processor = YoloV8PostProcessor {
            detection_parallel_threshold,
            nms_prescreen_multiplier,
            ..Default::default()
        };
        group.bench_with_input(BenchmarkId::new("scenario", name), &output, |b, out| {
            b.iter(|| {
                let result = processor
                    .process(out, &coord, &labels)
                    .expect("yolov8 postprocess matrix benchmark should succeed");
                black_box(result.detections.len());
            })
        });
    }

    group.finish();
}

fn bench_yolov5_postprocess(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_yolov5");
    let labels: Vec<String> = (0..80).map(|i| format!("class_{i}")).collect();
    let coord = make_coord_transform();
    let processor = YoloV5PostProcessor::default();
    for &preds in &[8400usize, 25200usize] {
        let output = make_yolov5_output(80, preds);
        group.throughput(Throughput::Elements(preds as u64));
        group.bench_with_input(BenchmarkId::new("preds", preds), &output, |b, out| {
            b.iter(|| {
                let result = processor
                    .process(out, &coord, &labels)
                    .expect("yolov5 postprocess should succeed");
                black_box(result.detections.len());
            })
        });
    }
    group.finish();
}

fn bench_yolov5_threshold_matrix(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_yolov5_threshold_matrix");
    let labels: Vec<String> = (0..80).map(|i| format!("class_{i}")).collect();
    let coord = make_coord_transform();
    let preds = 25_200usize;
    let output = make_yolov5_output(80, preds);
    group.throughput(Throughput::Elements(preds as u64));

    let scenarios: [(&str, usize, usize); 4] = [
        ("baseline", 8_192, 8),
        ("force_parallel", 1, 8),
        ("aggressive_prescreen", 8_192, 4),
        ("conservative_prescreen", 8_192, 16),
    ];

    for (name, detection_parallel_threshold, nms_prescreen_multiplier) in scenarios {
        let processor = YoloV5PostProcessor {
            detection_parallel_threshold,
            nms_prescreen_multiplier,
            ..Default::default()
        };
        group.bench_with_input(BenchmarkId::new("scenario", name), &output, |b, out| {
            b.iter(|| {
                let result = processor
                    .process(out, &coord, &labels)
                    .expect("yolov5 postprocess matrix benchmark should succeed");
                black_box(result.detections.len());
            })
        });
    }

    group.finish();
}

fn bench_yolov5_ab_compare(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_yolov5_ab_compare");
    let labels: Vec<String> = (0..80).map(|i| format!("class_{i}")).collect();
    let coord = make_coord_transform();
    let optimized = YoloV5PostProcessor::default();
    let legacy = LegacyYoloV5PostProcessor::default();
    let chunked = ChunkedYoloV5PostProcessor::default();

    for &preds in &[8400usize, 25200usize] {
        let output = make_yolov5_output(80, preds);
        group.throughput(Throughput::Elements(preds as u64));

        group.bench_with_input(
            BenchmarkId::new("legacy_preds", preds),
            &output,
            |b, out| {
                b.iter(|| {
                    let result = legacy
                        .process(out, &coord, &labels)
                        .expect("legacy yolov5 postprocess should succeed");
                    black_box(result.detections.len());
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("optimized_preds", preds),
            &output,
            |b, out| {
                b.iter(|| {
                    let result = optimized
                        .process(out, &coord, &labels)
                        .expect("optimized yolov5 postprocess should succeed");
                    black_box(result.detections.len());
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("chunked_preds", preds),
            &output,
            |b, out| {
                b.iter(|| {
                    let result = chunked
                        .process(out, &coord, &labels)
                        .expect("chunked yolov5 postprocess should succeed");
                    black_box(result.detections.len());
                })
            },
        );
    }

    group.finish();
}

fn bench_segmentation_postprocess(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_segmentation");
    let processor = SegmentationPostProcessor::default();
    let labels: Vec<String> = (0..32).map(|i| format!("seg_{i}")).collect();
    for &(classes, h, w) in &[(21usize, 512usize, 512usize), (32usize, 640usize, 640usize)] {
        let output = make_segmentation_output(classes, h, w);
        group.throughput(Throughput::Elements((h * w) as u64));
        group.bench_with_input(
            BenchmarkId::new("classes_hw", format!("{classes}_{h}x{w}")),
            &output,
            |b, out| {
                b.iter(|| {
                    let result = processor
                        .process(out, &make_coord_transform(), &labels)
                        .expect("segmentation postprocess should succeed");
                    black_box(result.segmentation_masks[0].mask.len());
                })
            },
        );
    }
    group.finish();
}

fn bench_classification_postprocess(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_classification");
    let processor = ClassificationPostProcessor::default();
    let labels: Vec<String> = (0..1000).map(|i| format!("label_{i}")).collect();
    for &classes in &[1000usize, 5000usize] {
        let output = make_classification_output(classes);
        group.throughput(Throughput::Elements(classes as u64));
        group.bench_with_input(BenchmarkId::new("classes", classes), &output, |b, out| {
            b.iter(|| {
                let result = processor
                    .process(out, &make_coord_transform(), &labels)
                    .expect("classification postprocess should succeed");
                black_box(result.classifications[0].top_k.len());
            })
        });
    }
    group.finish();
}

fn bench_classification_small_class_fast_path_matrix(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_classification_small_class_fast_path_matrix");
    let labels: Vec<String> = (0..5000).map(|i| format!("label_{i}")).collect();
    let classes = 5_000usize;
    let output = make_classification_output(classes);
    group.throughput(Throughput::Elements(classes as u64));

    for &fast_path in &[8usize, 16usize, 64usize, 128usize] {
        let processor = ClassificationPostProcessor {
            small_class_fast_path: fast_path,
            ..Default::default()
        };
        group.bench_with_input(
            BenchmarkId::new("small_class_fast_path", fast_path),
            &output,
            |b, out| {
                b.iter(|| {
                    let result = processor
                        .process(out, &make_coord_transform(), &labels)
                        .expect("classification matrix benchmark should succeed");
                    black_box(result.classifications[0].top_k.len());
                })
            },
        );
    }

    group.finish();
}

fn bench_segmentation_parallel_min_pixels_matrix(c: &mut Criterion) {
    let mut group = c.benchmark_group("postprocess_segmentation_parallel_min_pixels_matrix");
    let labels: Vec<String> = (0..32).map(|i| format!("seg_{i}")).collect();
    let output = make_segmentation_output(32, 640, 640);
    let pixels = 640usize * 640usize;
    group.throughput(Throughput::Elements(pixels as u64));

    for &parallel_min_pixels in &[8_192usize, 16_384usize, 65_536usize, 262_144usize] {
        let processor = SegmentationPostProcessor {
            parallel_min_pixels,
        };
        group.bench_with_input(
            BenchmarkId::new("parallel_min_pixels", parallel_min_pixels),
            &output,
            |b, out| {
                b.iter(|| {
                    let result = processor
                        .process(out, &make_coord_transform(), &labels)
                        .expect("segmentation matrix benchmark should succeed");
                    black_box(result.segmentation_masks[0].mask.len());
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = postprocess_kernel_benches;
    config = Criterion::default().sample_size(20);
    targets =
        bench_yolov8_postprocess,
        bench_yolov8_threshold_matrix,
        bench_yolov5_postprocess,
        bench_yolov5_threshold_matrix,
        bench_yolov5_ab_compare,
        bench_segmentation_postprocess,
        bench_segmentation_parallel_min_pixels_matrix,
        bench_classification_postprocess,
        bench_classification_small_class_fast_path_matrix
);
criterion_main!(postprocess_kernel_benches);
