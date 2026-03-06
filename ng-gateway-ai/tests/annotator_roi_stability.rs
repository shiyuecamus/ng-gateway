#![cfg(feature = "engine")]

use bytes::Bytes;
use ng_gateway_ai::{
    pipeline::{
        annotator::{DefaultFrameAnnotator, FrameAnnotator},
        roi::crop_frame,
    },
    DecodedFrame,
};
use ng_gateway_models::{
    domain::prelude::{AnalysisCore, BoundingBox, Detection},
    entities::ai::pipeline::{AnnotationConfig, RegionOfInterest},
};

/// Build deterministic frame data for repeatable regression tests.
fn make_frame(width: u32, height: u32) -> DecodedFrame {
    let len = (width as usize) * (height as usize) * 3;
    let mut data = Vec::with_capacity(len);
    for i in 0..len {
        data.push(((i * 13 + 5) % 256) as u8);
    }
    DecodedFrame::from_rgb24(Bytes::from(data), width, height)
}

#[test]
fn roi_crop_and_annotation_repeat_stable() {
    let frame = make_frame(1920, 1080);
    let rois = [
        RegionOfInterest {
            x_min: 0.0,
            y_min: 0.0,
            x_max: 0.5,
            y_max: 0.5,
        },
        RegionOfInterest {
            x_min: 0.5,
            y_min: 0.0,
            x_max: 1.0,
            y_max: 0.5,
        },
        RegionOfInterest {
            x_min: 0.0,
            y_min: 0.5,
            x_max: 0.5,
            y_max: 1.0,
        },
        RegionOfInterest {
            x_min: 0.5,
            y_min: 0.5,
            x_max: 1.0,
            y_max: 1.0,
        },
    ];

    let analysis = AnalysisCore {
        detections: vec![Detection {
            bbox: BoundingBox {
                x_min: 0.2,
                y_min: 0.2,
                x_max: 0.6,
                y_max: 0.6,
            },
            class: "person".into(),
            class_id: 0,
            confidence: 0.92,
            track_id: Some(7),
        }]
        .into(),
        ..Default::default()
    };
    let cfg = AnnotationConfig {
        max_output_dimension: Some(640),
        ..Default::default()
    };
    let annotator = DefaultFrameAnnotator;

    for _ in 0..32 {
        for roi in rois.iter() {
            let cropped = crop_frame(&frame, roi).expect("crop must succeed");
            let output = annotator
                .annotate(&cropped, &analysis, &cfg)
                .expect("annotation must succeed");
            assert!(!output.is_empty(), "annotated jpeg should not be empty");
        }
    }
}
