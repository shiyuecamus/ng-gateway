#![cfg(feature = "engine")]

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use fast_image_resize::{images::Image, PixelType, Resizer};
use ng_gateway_ai::{
    pipeline::preprocess::{
        CenterCropPreProcessor, DirectResizePreProcessor, LetterboxPreProcessor, PreProcessor,
        PreprocessInput,
    },
    DecodedFrame,
};
use ng_gateway_models::enums::ai::TensorDType;
use std::hint::black_box;

/// Build a deterministic RGB frame payload for benchmark reproducibility.
fn make_frame(width: u32, height: u32) -> DecodedFrame {
    let len = (width as usize) * (height as usize) * 3;
    let mut data = Vec::with_capacity(len);
    for i in 0..len {
        data.push(((i * 37 + 17) % 256) as u8);
    }
    DecodedFrame {
        data: Bytes::from(data),
        width,
        height,
    }
}

fn resize_rgb(frame: &DecodedFrame, new_w: u32, new_h: u32) -> Vec<u8> {
    let src = Image::from_vec_u8(
        frame.width,
        frame.height,
        frame.data.as_ref().to_vec(),
        PixelType::U8x3,
    )
    .expect("create source image");
    let mut dst = Image::new(new_w, new_h, PixelType::U8x3);
    let mut resizer = Resizer::new();
    resizer
        .resize(&src, &mut dst, None)
        .expect("resize image should succeed");
    dst.into_vec()
}

fn write_naive_scalar_rgb_nchw(pixels: &[u8], width: usize, height: usize, out: &mut [f32]) {
    let plane_size = width * height;
    let inv_255 = 1.0 / 255.0;
    for y in 0..height {
        for x in 0..width {
            let src_idx = (y * width + x) * 3;
            let dst_idx = y * width + x;
            out[dst_idx] = pixels[src_idx] as f32 * inv_255;
            out[plane_size + dst_idx] = pixels[src_idx + 1] as f32 * inv_255;
            out[2 * plane_size + dst_idx] = pixels[src_idx + 2] as f32 * inv_255;
        }
    }
}

fn build_lut() -> [[f32; 256]; 3] {
    let mut lut = [[0.0f32; 256]; 3];
    for table in &mut lut {
        for (v, mapped) in table.iter_mut().enumerate() {
            *mapped = v as f32 * (1.0 / 255.0);
        }
    }
    lut
}

fn write_lut_rgb_nchw(
    pixels: &[u8],
    width: usize,
    height: usize,
    out: &mut [f32],
    lut: &[[f32; 256]; 3],
) {
    let plane_size = width * height;
    for y in 0..height {
        for x in 0..width {
            let src_idx = (y * width + x) * 3;
            let dst_idx = y * width + x;
            out[dst_idx] = lut[0][pixels[src_idx] as usize];
            out[plane_size + dst_idx] = lut[1][pixels[src_idx + 1] as usize];
            out[2 * plane_size + dst_idx] = lut[2][pixels[src_idx + 2] as usize];
        }
    }
}

fn bench_preprocess_stage_breakdown(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess_stage_breakdown_640");
    let frame = make_frame(1920, 1080);
    let target_w = 640usize;
    let target_h = 640usize;
    let resized = resize_rgb(&frame, target_w as u32, target_h as u32);
    let mut output = vec![0.0f32; target_w * target_h * 3];
    let lut = build_lut();

    group.throughput(Throughput::Bytes(frame.data.len() as u64));
    group.bench_function("resize_only_1920x1080_to_640", |b| {
        b.iter_batched(
            || (),
            |_| {
                let out = resize_rgb(&frame, target_w as u32, target_h as u32);
                black_box(out.len());
            },
            BatchSize::SmallInput,
        )
    });

    group.throughput(Throughput::Bytes(resized.len() as u64));
    group.bench_function("write_only_naive_scalar_640", |b| {
        b.iter(|| {
            write_naive_scalar_rgb_nchw(&resized, target_w, target_h, &mut output);
            black_box(output[0]);
        })
    });

    group.bench_function("write_only_lut_640", |b| {
        b.iter(|| {
            write_lut_rgb_nchw(&resized, target_w, target_h, &mut output, &lut);
            black_box(output[0]);
        })
    });

    group.finish();
}

fn bench_letterbox(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess_letterbox");
    let frame = make_frame(1920, 1080);
    let processor = LetterboxPreProcessor::default();
    let shape = [1_i64, 3, 640, 640];
    group.throughput(Throughput::Bytes(frame.data.len() as u64));
    group.bench_function("rgb_1920x1080_to_640x640", |b| {
        b.iter_batched(
            || (),
            |_| {
                let output = processor
                    .process(PreprocessInput {
                        frame: &frame,
                        model_input_shape: &shape,
                        model_input_dtype: TensorDType::Float32,
                    })
                    .expect("letterbox preprocess should succeed");
                black_box(output.tensor.shape());
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_direct_resize(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess_direct_resize");
    let frame_hd = make_frame(1920, 1080);
    let frame_fhd = make_frame(1280, 720);
    let processor = DirectResizePreProcessor::default();
    let shape_rgb_640 = [1_i64, 3, 640, 640];
    let shape_gray_640 = [1_i64, 1, 640, 640];

    for (name, frame, shape) in [
        ("rgb_1920x1080_to_640x640", &frame_hd, &shape_rgb_640),
        ("rgb_1280x720_to_640x640", &frame_fhd, &shape_rgb_640),
        ("gray_1280x720_to_640x640", &frame_fhd, &shape_gray_640),
    ] {
        group.throughput(Throughput::Bytes(frame.data.len() as u64));
        group.bench_with_input(BenchmarkId::new("path", name), shape, |b, model_shape| {
            b.iter_batched(
                || (),
                |_| {
                    let output = processor
                        .process(PreprocessInput {
                            frame,
                            model_input_shape: model_shape,
                            model_input_dtype: TensorDType::Float32,
                        })
                        .expect("direct_resize preprocess should succeed");
                    black_box(output.tensor.shape());
                },
                BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

fn bench_center_crop(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess_center_crop");
    let frame = make_frame(1920, 1080);
    let processor = CenterCropPreProcessor::default();
    let shape = [1_i64, 3, 224, 224];
    group.throughput(Throughput::Bytes(frame.data.len() as u64));
    group.bench_function("rgb_1920x1080_to_224x224", |b| {
        b.iter_batched(
            || (),
            |_| {
                let output = processor
                    .process(PreprocessInput {
                        frame: &frame,
                        model_input_shape: &shape,
                        model_input_dtype: TensorDType::Float32,
                    })
                    .expect("center_crop preprocess should succeed");
                black_box(output.tensor.shape());
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(
    name = preprocess_kernel_benches;
    config = Criterion::default().sample_size(20);
    targets = bench_preprocess_stage_breakdown, bench_letterbox, bench_direct_resize, bench_center_crop
);
criterion_main!(preprocess_kernel_benches);
