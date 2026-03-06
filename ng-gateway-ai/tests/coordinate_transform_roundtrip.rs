//! Integration tests: coordinate transform precision and stability.
//!
//! Validates that `CoordinateTransform::map_point_to_original` correctly
//! reverses the geometric operations (letterbox padding, direct resize scaling)
//! applied during preprocessing.

#![cfg(feature = "engine")]

use bytes::Bytes;
use ng_gateway_ai::pipeline::preprocess::*;
use ng_gateway_ai::DecodedFrame;
use ng_gateway_models::enums::ai::TensorDType;

// ── Helpers ──────────────────────────────────────────────────────────

fn make_solid_frame(width: u32, height: u32, r: u8, g: u8, b: u8) -> DecodedFrame {
    let pixel_count = width as usize * height as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for _ in 0..pixel_count {
        data.extend_from_slice(&[r, g, b]);
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

// ── Test 1: Letterbox center point roundtrip ─────────────────────────

#[test]
fn letterbox_transform_center_point_roundtrip() {
    let frame = make_solid_frame(1920, 1080, 128, 128, 128);
    let preprocessor = LetterboxPreProcessor::default();
    let model_shape: [i64; 4] = [1, 3, 640, 640];

    let output = preprocessor
        .process(PreprocessInput {
            frame: &frame,
            model_input_shape: &model_shape,
            model_input_dtype: TensorDType::Float32,
        })
        .expect("letterbox preprocess should succeed");

    let ct = output.coord_transform();

    // Center of model input space (0.5, 0.5) should map to approximately
    // the center of the original frame.
    let (ox, oy) = ct.map_point_to_original(0.5, 0.5);

    // For 1920x1080 → 640x640 letterbox:
    //   scale = 640/1920 ≈ 0.3333
    //   new_w = 640, new_h = 360
    //   pad_x = 0, pad_y = 140
    // (0.5 * 640 - 0) / 0.3333 / 1920 ≈ 0.5
    // (0.5 * 640 - 140) / 0.3333 / 1080 ≈ 0.5
    assert!(
        (ox - 0.5).abs() < 0.02,
        "center x should map to ~0.5, got {ox}"
    );
    assert!(
        (oy - 0.5).abs() < 0.02,
        "center y should map to ~0.5, got {oy}"
    );
}

// ── Test 2: DirectResize preserves corner extremes ───────────────────

#[test]
fn direct_resize_asymmetric_preserves_extremes() {
    let frame = make_solid_frame(1920, 1080, 64, 64, 64);
    let preprocessor = DirectResizePreProcessor::default();
    let model_shape: [i64; 4] = [1, 3, 640, 640];

    let output = preprocessor
        .process(PreprocessInput {
            frame: &frame,
            model_input_shape: &model_shape,
            model_input_dtype: TensorDType::Float32,
        })
        .expect("direct resize should succeed");

    let ct = output.coord_transform();

    // DirectResize has no padding, only scaling. The coordinate transform
    // should map (0,0) → (0,0) and (1,1) → (1,1) in original-frame space
    // because the scaling is applied uniformly to both axes independently.
    let (ox_min, oy_min) = ct.map_point_to_original(0.0, 0.0);
    let (ox_max, oy_max) = ct.map_point_to_original(1.0, 1.0);

    assert!(
        (ox_min - 0.0).abs() < 1e-5,
        "origin x should map to 0.0, got {ox_min}"
    );
    assert!(
        (oy_min - 0.0).abs() < 1e-5,
        "origin y should map to 0.0, got {oy_min}"
    );
    assert!(
        (ox_max - 1.0).abs() < 1e-5,
        "max x should map to 1.0, got {ox_max}"
    );
    assert!(
        (oy_max - 1.0).abs() < 1e-5,
        "max y should map to 1.0, got {oy_max}"
    );
}

// ── Test 3: Multiple transform compositions remain in [0,1] ─────────

#[test]
fn multiple_transform_compositions_stable() {
    let resolutions = [
        (1920, 1080),
        (1280, 720),
        (640, 480),
        (3840, 2160),
        (320, 240),
        (800, 600),
    ];
    let preprocessors: Vec<Box<dyn PreProcessor>> = vec![
        Box::new(LetterboxPreProcessor::default()),
        Box::new(DirectResizePreProcessor::default()),
        Box::new(CenterCropPreProcessor::default()),
    ];
    let model_shape: [i64; 4] = [1, 3, 640, 640];

    // Grid of test points covering the model-input normalized space.
    let test_points: Vec<(f32, f32)> = {
        let mut pts = Vec::with_capacity(121);
        for ix in 0..=10 {
            for iy in 0..=10 {
                pts.push((ix as f32 / 10.0, iy as f32 / 10.0));
            }
        }
        pts
    };

    for (width, height) in resolutions {
        let frame = make_solid_frame(width, height, 100, 100, 100);
        for preprocessor in &preprocessors {
            let output = match preprocessor.process(PreprocessInput {
                frame: &frame,
                model_input_shape: &model_shape,
                model_input_dtype: TensorDType::Float32,
            }) {
                Ok(o) => o,
                Err(_) => continue,
            };

            let ct = output.coord_transform();

            for &(px, py) in &test_points {
                let (ox, oy) = ct.map_point_to_original(px, py);
                assert!(
                    (0.0..=1.0).contains(&ox),
                    "[{} @ {}x{}] point ({px},{py}) mapped x={ox} out of [0,1]",
                    preprocessor.name(),
                    width,
                    height,
                );
                assert!(
                    (0.0..=1.0).contains(&oy),
                    "[{} @ {}x{}] point ({px},{py}) mapped y={oy} out of [0,1]",
                    preprocessor.name(),
                    width,
                    height,
                );
            }
        }
    }
}

// ── Test 4: Letterbox vertical frame (portrait) ──────────────────────

#[test]
fn letterbox_vertical_frame_correct_padding() {
    let frame = make_solid_frame(480, 1920, 128, 128, 128);
    let preprocessor = LetterboxPreProcessor::default();
    let model_shape: [i64; 4] = [1, 3, 640, 640];

    let output = preprocessor
        .process(PreprocessInput {
            frame: &frame,
            model_input_shape: &model_shape,
            model_input_dtype: TensorDType::Float32,
        })
        .expect("letterbox should handle portrait frames");

    let ct = output.coord_transform();

    // Portrait: height dominates → scale = 640/1920, pad_x > 0, pad_y ≈ 0.
    assert!(
        ct.pad_x > 0.0,
        "portrait frame should have horizontal padding"
    );

    // Center should still map to center.
    let (ox, oy) = ct.map_point_to_original(0.5, 0.5);
    assert!(
        (ox - 0.5).abs() < 0.02,
        "center x should map to ~0.5, got {ox}"
    );
    assert!(
        (oy - 0.5).abs() < 0.02,
        "center y should map to ~0.5, got {oy}"
    );
}
