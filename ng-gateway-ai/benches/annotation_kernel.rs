#![cfg(feature = "engine")]

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use ng_gateway_ai::{
    pipeline::annotator::{DefaultFrameAnnotator, FrameAnnotator},
    DecodedFrame,
};
use ng_gateway_models::{
    domain::prelude::{AnalysisCore, BoundingBox, Detection},
    entities::ai::pipeline::AnnotationConfig,
};
use std::hint::black_box;

/// Build a deterministic RGB frame for benchmark reproducibility.
fn make_frame(width: u32, height: u32) -> DecodedFrame {
    let len = (width as usize) * (height as usize) * 3;
    let mut data = Vec::with_capacity(len);
    for i in 0..len {
        data.push(((i * 29 + 7) % 256) as u8);
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

/// Build synthetic detections with deterministic layout.
fn make_detections(count: usize) -> Vec<Detection> {
    (0..count)
        .map(|i| {
            let x = (i as f32 * 0.07) % 0.8;
            let y = (i as f32 * 0.11) % 0.8;
            Detection {
                bbox: BoundingBox {
                    x_min: x,
                    y_min: y,
                    x_max: (x + 0.15).min(1.0),
                    y_max: (y + 0.15).min(1.0),
                },
                class: if i % 2 == 0 {
                    "person".into()
                } else {
                    "car".into()
                },
                class_id: (i % 2) as u32,
                confidence: 0.9 - (i as f32 * 0.01).max(0.1),
                track_id: Some(i as u64),
            }
        })
        .collect()
}

fn bench_annotation(c: &mut Criterion) {
    let mut group = c.benchmark_group("annotation_kernel");
    let annotator = DefaultFrameAnnotator;
    let detections = make_detections(24);
    let analysis = AnalysisCore {
        detections: detections.into(),
        ..Default::default()
    };

    for (name, width, height, max_dim) in [
        ("1080p_no_resize", 1920_u32, 1080_u32, None),
        ("1080p_resize_960", 1920_u32, 1080_u32, Some(960_u32)),
        ("4k_resize_1280", 3840_u32, 2160_u32, Some(1280_u32)),
    ] {
        let frame = make_frame(width, height);
        let config = AnnotationConfig {
            max_output_dimension: max_dim,
            ..Default::default()
        };
        group.throughput(Throughput::Bytes(frame.pixel_data_size() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), &config, |b, cfg| {
            b.iter_batched(
                || (),
                |_| {
                    let out = annotator
                        .annotate(&frame, &analysis, cfg)
                        .expect("annotation benchmark should succeed");
                    black_box(out.len());
                },
                BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

criterion_group!(
    name = annotation_kernel_benches;
    config = Criterion::default().sample_size(20);
    targets = bench_annotation
);
criterion_main!(annotation_kernel_benches);
