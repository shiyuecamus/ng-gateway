//! Integration tests: ROI crop → analysis → coordinate remap pipeline.
//!
//! Verifies that cropping a region of interest, running analysis on the
//! sub-frame, and then remapping coordinates back to full-frame space
//! produces geometrically correct results.

#![cfg(feature = "engine")]

use bytes::Bytes;
use ng_gateway_ai::pipeline::preprocess::*;
use ng_gateway_ai::pipeline::roi::*;
use ng_gateway_ai::DecodedFrame;
use ng_gateway_models::domain::prelude::BoundingBox;
use ng_gateway_models::entities::ai::pipeline::RegionOfInterest;
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

fn make_gradient_frame(width: u32, height: u32) -> DecodedFrame {
    let pixel_count = width as usize * height as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for y in 0..height {
        for x in 0..width {
            data.push(((x * 255) / width.max(1)) as u8);
            data.push(((y * 255) / height.max(1)) as u8);
            data.push(128u8);
        }
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

/// Remap a normalized bbox from ROI-local space to full-frame space.
///
/// Mirrors the engine-internal `remap_bbox_to_full_frame` which is
/// `pub(super)` and not accessible from integration tests.
fn remap_bbox_to_full_frame(bbox: &BoundingBox, roi: &RegionOfInterest) -> BoundingBox {
    let remap = |v: f32, min: f32, max: f32| -> f32 { (min + v * (max - min)).clamp(0.0, 1.0) };
    BoundingBox {
        x_min: remap(bbox.x_min, roi.x_min, roi.x_max),
        y_min: remap(bbox.y_min, roi.y_min, roi.y_max),
        x_max: remap(bbox.x_max, roi.x_min, roi.x_max),
        y_max: remap(bbox.y_max, roi.y_min, roi.y_max),
    }
}

// ── Test 1: Crop right half → detection at center → remap ────────────

#[test]
fn crop_then_remap_preserves_global_position() {
    let frame = make_solid_frame(1920, 1080, 128, 128, 128);

    // ROI: right half of the frame.
    let roi = RegionOfInterest {
        x_min: 0.5,
        y_min: 0.0,
        x_max: 1.0,
        y_max: 1.0,
    };

    let cropped = crop_frame(&frame, &roi).expect("crop should succeed");
    assert_eq!(cropped.width, 960, "right half should be 960px wide");
    assert_eq!(cropped.height, 1080, "height should be preserved");

    // Simulate a detection at the center of the cropped sub-frame:
    // local normalized bbox centered at (0.5, 0.5) in ROI-local space.
    let local_bbox = BoundingBox {
        x_min: 0.4,
        y_min: 0.4,
        x_max: 0.6,
        y_max: 0.6,
    };

    // Remap to full-frame coordinates.
    let global_bbox = remap_bbox_to_full_frame(&local_bbox, &roi);

    // Expected: the local center (0.5, 0.5) maps to global (0.75, 0.5).
    // x: 0.5 + 0.4 * 0.5 = 0.7, 0.5 + 0.6 * 0.5 = 0.8
    // y: 0.0 + 0.4 * 1.0 = 0.4, 0.0 + 0.6 * 1.0 = 0.6
    let expected_x_center = (global_bbox.x_min + global_bbox.x_max) / 2.0;
    let expected_y_center = (global_bbox.y_min + global_bbox.y_max) / 2.0;

    assert!(
        (expected_x_center - 0.75).abs() < 0.01,
        "global x center should be ~0.75, got {expected_x_center}"
    );
    assert!(
        (expected_y_center - 0.5).abs() < 0.01,
        "global y center should be ~0.5, got {expected_y_center}"
    );

    // Verify all coordinates are within [0,1].
    assert!(global_bbox.x_min >= 0.0 && global_bbox.x_min <= 1.0);
    assert!(global_bbox.y_min >= 0.0 && global_bbox.y_min <= 1.0);
    assert!(global_bbox.x_max >= 0.0 && global_bbox.x_max <= 1.0);
    assert!(global_bbox.y_max >= 0.0 && global_bbox.y_max <= 1.0);
}

// ── Test 2: Full-frame ROI → remap is identity ──────────────────────

#[test]
fn full_frame_roi_remap_is_identity() {
    let roi = RegionOfInterest::FULL;
    let bbox = BoundingBox {
        x_min: 0.2,
        y_min: 0.3,
        x_max: 0.8,
        y_max: 0.9,
    };

    let remapped = remap_bbox_to_full_frame(&bbox, &roi);

    assert!(
        (remapped.x_min - bbox.x_min).abs() < 1e-6,
        "full-frame ROI remap should be identity for x_min"
    );
    assert!(
        (remapped.y_min - bbox.y_min).abs() < 1e-6,
        "full-frame ROI remap should be identity for y_min"
    );
    assert!(
        (remapped.x_max - bbox.x_max).abs() < 1e-6,
        "full-frame ROI remap should be identity for x_max"
    );
    assert!(
        (remapped.y_max - bbox.y_max).abs() < 1e-6,
        "full-frame ROI remap should be identity for y_max"
    );
}

// ── Test 3: Crop → preprocess → verify tensor shape ──────────────────

#[test]
fn cropped_frame_preprocesses_correctly() {
    let frame = make_gradient_frame(1920, 1080);

    let roi = RegionOfInterest {
        x_min: 0.25,
        y_min: 0.25,
        x_max: 0.75,
        y_max: 0.75,
    };

    let cropped = crop_frame(&frame, &roi).expect("crop should succeed");
    assert_eq!(cropped.width, 960);
    assert_eq!(cropped.height, 540);

    // Feed cropped frame through letterbox preprocessor.
    let preprocessor = LetterboxPreProcessor::default();
    let model_shape: [i64; 4] = [1, 3, 640, 640];

    let output = preprocessor
        .process(PreprocessInput {
            frame: &cropped,
            model_input_shape: &model_shape,
            model_input_dtype: TensorDType::Float32,
        })
        .expect("preprocess of cropped frame should succeed");

    let PreprocessOutput::CpuTensor { ref tensor, .. } = output else {
        panic!("expected CpuTensor");
    };
    assert_eq!(tensor.shape(), &[1, 3, 640, 640]);

    // The coordinate transform should refer to the cropped dimensions.
    let ct = output.coord_transform();
    assert_eq!(ct.orig_width, 960);
    assert_eq!(ct.orig_height, 540);
}

// ── Test 4: Bottom-left quadrant ROI remap ───────────────────────────

#[test]
fn bottom_left_roi_remap_correct() {
    let roi = RegionOfInterest {
        x_min: 0.0,
        y_min: 0.5,
        x_max: 0.5,
        y_max: 1.0,
    };

    // A detection spanning the full ROI sub-frame.
    let local_bbox = BoundingBox {
        x_min: 0.0,
        y_min: 0.0,
        x_max: 1.0,
        y_max: 1.0,
    };

    let global = remap_bbox_to_full_frame(&local_bbox, &roi);

    assert!((global.x_min - 0.0).abs() < 1e-6, "x_min should map to 0.0");
    assert!((global.y_min - 0.5).abs() < 1e-6, "y_min should map to 0.5");
    assert!((global.x_max - 0.5).abs() < 1e-6, "x_max should map to 0.5");
    assert!((global.y_max - 1.0).abs() < 1e-6, "y_max should map to 1.0");
}

// ── Test 5: Remap clamps out-of-range values ─────────────────────────

#[test]
fn remap_clamps_out_of_range_values() {
    let roi = RegionOfInterest {
        x_min: 0.8,
        y_min: 0.8,
        x_max: 1.0,
        y_max: 1.0,
    };

    let local_bbox = BoundingBox {
        x_min: -0.5,
        y_min: -0.3,
        x_max: 1.5,
        y_max: 1.2,
    };

    let global = remap_bbox_to_full_frame(&local_bbox, &roi);

    assert!(
        global.x_min >= 0.0 && global.x_min <= 1.0,
        "x_min out of [0,1]: {}",
        global.x_min
    );
    assert!(
        global.y_min >= 0.0 && global.y_min <= 1.0,
        "y_min out of [0,1]: {}",
        global.y_min
    );
    assert!(
        global.x_max >= 0.0 && global.x_max <= 1.0,
        "x_max out of [0,1]: {}",
        global.x_max
    );
    assert!(
        global.y_max >= 0.0 && global.y_max <= 1.0,
        "y_max out of [0,1]: {}",
        global.y_max
    );
}
