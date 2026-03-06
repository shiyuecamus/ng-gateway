#![cfg(feature = "engine")]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::hint::black_box;

use bytes::Bytes;
use ng_gateway_ai::{
    frame::pool::CpuBufferPool,
    pipeline::{
        postprocess::{nms_per_class_with_variant, NmsVariant},
        preprocess::CoordinateTransform,
        roi::crop_frame,
    },
    result::alarm::{evaluate_alarm_rules, evaluate_alarm_rules_with_history, TrackHistory},
    DecodedFrame,
};
use ng_gateway_models::{
    domain::prelude::{AlarmRuleInfo, BoundingBox, Detection},
    entities::ai::alarm_rule::AlarmCondition,
    enums::ai::AlarmSeverity,
};
use std::sync::Arc;

// ── Helpers ──────────────────────────────────────────────────────────

/// Build a solid-color RGB24 frame for benchmark reproducibility.
fn make_frame(w: u32, h: u32) -> DecodedFrame {
    let data = vec![128u8; w as usize * h as usize * 3];
    DecodedFrame::from_rgb24(Bytes::from(data), w, h)
}

/// Build a synthetic detection with deterministic spatial layout.
fn make_detection(class: &str, class_id: u32, idx: usize, total: usize) -> Detection {
    let step = 1.0 / (total as f32 + 1.0);
    let cx = step * (idx as f32 + 1.0);
    let half = step * 0.4;
    Detection {
        bbox: BoundingBox {
            x_min: (cx - half).max(0.0),
            y_min: 0.3,
            x_max: (cx + half).min(1.0),
            y_max: 0.7,
        },
        class: Arc::from(class),
        class_id,
        confidence: 0.9 - (idx as f32 * 0.001),
        track_id: None,
    }
}

/// Build a letterbox-style `CoordinateTransform` (1920×1080 → 640×640).
fn letterbox_transform() -> CoordinateTransform {
    let scale = 640.0_f32 / 1920.0;
    let new_h = (1080.0 * scale).round();
    let pad_y = ((640.0 - new_h) / 2.0).round();
    CoordinateTransform {
        scale_x: scale,
        scale_y: scale,
        pad_x: 0.0,
        pad_y,
        orig_width: 1920,
        orig_height: 1080,
        input_width: 640,
        input_height: 640,
    }
}

/// Build a minimal `AlarmRuleInfo` for benchmark use.
fn make_alarm_rule(id: i32, condition: AlarmCondition) -> AlarmRuleInfo {
    AlarmRuleInfo {
        id,
        name: format!("bench_rule_{id}"),
        pipeline_id: 1,
        rule_order: id,
        severity: AlarmSeverity::Warning,
        condition,
        cooldown_secs: 0,
        min_duration_secs: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

// ── 1. ROI cropping at different resolutions ─────────────────────────

fn bench_roi_crop(c: &mut Criterion) {
    use ng_gateway_models::entities::ai::pipeline::RegionOfInterest;

    let mut group = c.benchmark_group("roi_crop");

    let cases: &[(&str, u32, u32, RegionOfInterest)] = &[
        (
            "1080p_center_crop",
            1920,
            1080,
            RegionOfInterest {
                x_min: 0.25,
                y_min: 0.25,
                x_max: 0.75,
                y_max: 0.75,
            },
        ),
        (
            "4k_center_crop",
            3840,
            2160,
            RegionOfInterest {
                x_min: 0.25,
                y_min: 0.25,
                x_max: 0.75,
                y_max: 0.75,
            },
        ),
        (
            "1080p_small_crop",
            1920,
            1080,
            RegionOfInterest {
                x_min: 0.4,
                y_min: 0.4,
                x_max: 0.6,
                y_max: 0.6,
            },
        ),
    ];

    for (label, w, h, roi) in cases {
        let frame = make_frame(*w, *h);
        group.bench_with_input(BenchmarkId::from_parameter(label), roi, |b, roi| {
            b.iter(|| {
                let cropped = crop_frame(&frame, roi).expect("crop should succeed");
                black_box(cropped.width);
            })
        });
    }

    group.finish();
}

// ── 2. Batch bbox coordinate transform ───────────────────────────────

fn bench_coordinate_transform(c: &mut Criterion) {
    let mut group = c.benchmark_group("coordinate_transform");
    let ct = letterbox_transform();

    for count in [100, 1000] {
        let bboxes: Vec<BoundingBox> = (0..count)
            .map(|i| {
                let offset = i as f32 / count as f32;
                BoundingBox {
                    x_min: offset * 0.8,
                    y_min: 0.1,
                    x_max: offset * 0.8 + 0.15,
                    y_max: 0.6,
                }
            })
            .collect();

        group.bench_with_input(BenchmarkId::new("map_bbox", count), &bboxes, |b, bboxes| {
            b.iter(|| {
                for bbox in bboxes {
                    black_box(ct.map_bbox_to_original(bbox));
                }
            })
        });
    }

    group.finish();
}

// ── 3. NMS with different detection counts ───────────────────────────

fn bench_nms_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("nms_scaling");
    group.sample_size(50);

    for &count in &[100usize, 500, 1000, 5000] {
        // Generate detections across 2 classes with moderate spatial overlap.
        let raw: Vec<(Detection, f32)> = (0..count)
            .map(|i| {
                let class_id = (i % 2) as u32;
                let class = if class_id == 0 { "person" } else { "car" };
                let step = 1.0 / (count as f32).sqrt();
                let row = i / (count as f32).sqrt() as usize;
                let col = i % (count as f32).sqrt() as usize;
                let x = col as f32 * step;
                let y = row as f32 * step;
                let size = step * 1.5; // moderate overlap
                let conf = 0.95 - (i as f32 * 0.0001);
                let det = Detection {
                    bbox: BoundingBox {
                        x_min: x.max(0.0),
                        y_min: y.max(0.0),
                        x_max: (x + size).min(1.0),
                        y_max: (y + size).min(1.0),
                    },
                    class: Arc::from(class),
                    class_id,
                    confidence: conf.max(0.1),
                    track_id: None,
                };
                (det, conf.max(0.1))
            })
            .collect();

        group.bench_with_input(BenchmarkId::new("classic", count), &raw, |b, raw| {
            b.iter(|| {
                let result =
                    nms_per_class_with_variant(raw.clone(), 0.5, 300, NmsVariant::Classic, 8);
                black_box(result.len());
            })
        });

        group.bench_with_input(BenchmarkId::new("diou", count), &raw, |b, raw| {
            b.iter(|| {
                let result = nms_per_class_with_variant(raw.clone(), 0.5, 300, NmsVariant::DIoU, 8);
                black_box(result.len());
            })
        });
    }

    group.finish();
}

// ── 4. Alarm rule evaluation ─────────────────────────────────────────

fn bench_alarm_evaluation(c: &mut Criterion) {
    use ng_gateway_ai::pipeline::context::PipelineContext;

    let mut group = c.benchmark_group("alarm_evaluation");

    // 100 detections spread across 3 classes.
    let detections: Vec<Detection> = (0..100)
        .map(|i| {
            let class = match i % 3 {
                0 => "person",
                1 => "car",
                _ => "bicycle",
            };
            make_detection(class, (i % 3) as u32, i, 100)
        })
        .collect();

    let frame = make_frame(640, 480);
    let mut context = PipelineContext::new(frame);
    context.detections = detections;

    // 5 alarm rules of mixed types.
    let rules = vec![
        make_alarm_rule(
            1,
            AlarmCondition::ClassDetected {
                class: "person".into(),
                min_confidence: 0.5,
            },
        ),
        make_alarm_rule(
            2,
            AlarmCondition::ClassDetected {
                class: "car".into(),
                min_confidence: 0.7,
            },
        ),
        make_alarm_rule(
            3,
            AlarmCondition::CountExceeds {
                class: Some("person".into()),
                threshold: 10,
            },
        ),
        make_alarm_rule(
            4,
            AlarmCondition::ZoneIntrusion {
                zone: vec![(0.1, 0.1), (0.9, 0.1), (0.9, 0.9), (0.1, 0.9)],
                class: None,
            },
        ),
        make_alarm_rule(
            5,
            AlarmCondition::CountExceeds {
                class: None,
                threshold: 50,
            },
        ),
    ];

    group.bench_function("100_dets_5_rules_no_history", |b| {
        b.iter(|| {
            let alarms = evaluate_alarm_rules(&rules, &context);
            black_box(alarms.len());
        })
    });

    // With TrackHistory (add track IDs to detections).
    let tracked_frame = make_frame(640, 480);
    let mut tracked_context = PipelineContext::new(tracked_frame);
    tracked_context.detections = (0..100)
        .map(|i| {
            let class = match i % 3 {
                0 => "person",
                1 => "car",
                _ => "bicycle",
            };
            let mut det = make_detection(class, (i % 3) as u32, i, 100);
            det.track_id = Some(i as u64);
            det
        })
        .collect();

    let mut history = TrackHistory::default();
    history.update_from_context(&tracked_context);

    group.bench_function("100_dets_5_rules_with_history", |b| {
        b.iter(|| {
            let alarms = evaluate_alarm_rules_with_history(&rules, &tracked_context, &history);
            black_box(alarms.len());
        })
    });

    group.finish();
}

// ── 5. CpuBufferPool checkout/return throughput ──────────────────────

fn bench_buffer_pool(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_pool");

    let pool = Arc::new(CpuBufferPool::for_resolution(8, 1920, 1080, 3));

    group.bench_function("checkout_return_cycle", |b| {
        b.iter(|| {
            let mut buf = pool.checkout();
            buf.as_mut_vec().extend_from_slice(&[42u8; 256]);
            black_box(buf.len());
            drop(buf);
        })
    });

    group.bench_function("burst_8_checkout_return", |b| {
        b.iter(|| {
            let bufs: Vec<_> = (0..8).map(|_| pool.checkout()).collect();
            for buf in &bufs {
                black_box(buf.capacity());
            }
            drop(bufs);
        })
    });

    group.finish();
}

// ── Criterion harness ────────────────────────────────────────────────

criterion_group!(
    name = pipeline_kernel_benches;
    config = Criterion::default().sample_size(50);
    targets =
        bench_roi_crop,
        bench_coordinate_transform,
        bench_nms_scaling,
        bench_alarm_evaluation,
        bench_buffer_pool
);
criterion_main!(pipeline_kernel_benches);
