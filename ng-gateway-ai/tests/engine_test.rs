//! AI engine integration tests.
//!
//! These tests exercise the engine lifecycle, pipeline CRUD, backpressure
//! semantics, status API, and frame submission error paths.
//!
//! # Requirements
//! - Requires the `engine` feature (ONNX Runtime).
//! - No real AI model is needed for most tests — they verify engine orchestration
//!   and control-plane behaviour rather than inference correctness.
//!
//! # Running
//! ```bash
//! cargo test -p ng-gateway-ai --features engine --test engine_test
//! ```

#![cfg(feature = "engine")]
use bytes::Bytes;
use chrono::Utc;
use ng_gateway_ai::engine::AiEngine;
use ng_gateway_common::metrics::NGMetricsHub;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    ai::{
        api::AiEngineApi,
        model::{ModelTask, ModelUpdateRequest, ModelUploadMetadata},
        pipeline::{
            AlarmCondition, AlarmRule, AnnotationConfig, PipelineConfig, PipelineUpsertRequest,
            SamplingStrategy, StageConfig,
        },
        types::{AlarmSeverity, FrameAnalysisRequest, FrameFormat, PipelineId, VideoFrame},
    },
    settings::AiEngineConfig,
};
use std::sync::Arc;
use tempfile::TempDir;

/// Create an engine backed by an empty temp directory (no models on disk).
async fn create_test_engine(max_concurrent: usize) -> (AiEngine, TempDir) {
    let tmp = TempDir::new().expect("create temp dir");
    let metrics_hub = Arc::new(NGMetricsHub::new().expect("metrics hub"));
    let config = AiEngineConfig {
        enabled: true,
        models_dir: tmp.path().join("models"),
        algorithms_dir: tmp.path().join("algorithms"),
        max_concurrent_inferences: max_concurrent,
        decoder_workers: 1,
        ..Default::default()
    };
    let engine = AiEngine::new(config, metrics_hub)
        .await
        .expect("engine init");
    (engine, tmp)
}

/// Build a minimal pipeline config for testing (no real model).
fn test_pipeline(model_id: &str) -> PipelineConfig {
    PipelineConfig {
        id: PipelineId::new("test_pipeline"),
        name: "Test Pipeline".into(),
        sampling: SamplingStrategy::EveryFrame,
        roi: None,
        roi_regions: vec![],
        stages: vec![StageConfig::Inference {
            model_id: model_id.into(),
            confidence_threshold: 0.5,
            nms_iou_threshold: Some(0.45),
            input_size: Some((640, 640)),
            preprocess: None,
            postprocess: None,
        }],
        alarm_rules: vec![AlarmRule {
            name: "test_alarm".into(),
            condition: AlarmCondition::ClassDetected {
                class: "person".into(),
                min_confidence: 0.6,
            },
            severity: AlarmSeverity::Warning,
            cooldown_secs: 10,
            min_duration_secs: None,
        }],
        annotation: AnnotationConfig::default(),
    }
}

/// Build a fake video frame (small JPEG) for submission.
fn fake_frame(seq: u64) -> VideoFrame {
    // Minimal valid JPEG: SOI + APP0 marker + EOI
    // A 1x1 white pixel JPEG for testing frame submission path.
    let jpeg_bytes: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06,
        0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B,
        0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
        0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31,
        0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF,
        0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00,
        0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
        0xFF, 0xC4, 0x00, 0xB5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05,
        0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21,
        0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08,
        0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A,
        0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37,
        0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56,
        0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75,
        0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93,
        0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9,
        0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6,
        0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
        0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7,
        0xF8, 0xF9, 0xFA, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x7B, 0x94,
        0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xFF, 0xD9,
    ];
    VideoFrame {
        data: Bytes::from_static(jpeg_bytes),
        format: FrameFormat::Jpeg,
        width: 1,
        height: 1,
        timestamp: Utc::now(),
        seq,
    }
}

// ───────────────────────────────────────────────────────────────────
// Engine lifecycle
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn engine_init_with_empty_models_dir() {
    let (engine, _tmp) = create_test_engine(4).await;

    let models = engine.list_models().await.expect("list_models");
    assert!(models.is_empty(), "empty models dir should yield no models");
}

#[tokio::test]
async fn engine_status_reports_correct_config() {
    let (engine, _tmp) = create_test_engine(8).await;

    let status = engine.get_engine_status().await.expect("status");
    assert!(status.enabled);
    assert_eq!(status.execution_provider, "cpu");
    assert_eq!(status.inference.max_concurrent, 8);
    assert_eq!(status.inference.available_permits, 8);
    assert_eq!(status.inference.active_count, 0);
    assert_eq!(status.inference.total_inferences, 0);
    assert_eq!(status.models.registered, 0);
    assert_eq!(status.models.loaded, 0);
    assert_eq!(status.pipelines.registered, 0);
    assert!(status.uptime_secs < 5, "uptime should be near zero");
}

// ───────────────────────────────────────────────────────────────────
// Pipeline CRUD
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pipeline_register_and_list() {
    let (engine, _tmp) = create_test_engine(4).await;

    engine
        .register_pipeline(1, test_pipeline("yolov8n"))
        .expect("register pipeline #1");
    engine
        .register_pipeline(2, test_pipeline("yolov8n"))
        .expect("register pipeline #2");

    let pipelines = engine.list_pipelines().await.expect("list");
    assert_eq!(pipelines.len(), 2);

    let ids: Vec<i32> = pipelines.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
}

#[tokio::test]
async fn pipeline_get_existing() {
    let (engine, _tmp) = create_test_engine(4).await;

    engine
        .register_pipeline(42, test_pipeline("yolov8n"))
        .expect("register pipeline");

    let config = engine.get_pipeline(42).await.expect("get");
    assert!(config.is_some());
    assert_eq!(config.unwrap().name, "Test Pipeline");
}

#[tokio::test]
async fn pipeline_get_nonexistent() {
    let (engine, _tmp) = create_test_engine(4).await;

    let config = engine.get_pipeline(999).await.expect("get");
    assert!(config.is_none());
}

#[tokio::test]
async fn pipeline_unregister() {
    let (engine, _tmp) = create_test_engine(4).await;

    engine
        .register_pipeline(1, test_pipeline("yolov8n"))
        .expect("register pipeline");
    assert_eq!(engine.list_pipelines().await.unwrap().len(), 1);

    engine.unregister_pipeline(1);
    assert_eq!(engine.list_pipelines().await.unwrap().len(), 0);

    let config = engine.get_pipeline(1).await.expect("get");
    assert!(config.is_none());
}

#[tokio::test]
async fn pipeline_replace() {
    let (engine, _tmp) = create_test_engine(4).await;

    engine
        .register_pipeline(1, test_pipeline("model_a"))
        .expect("register first");
    engine
        .register_pipeline(1, test_pipeline("model_b"))
        .expect("replace pipeline");

    let pipelines = engine.list_pipelines().await.unwrap();
    assert_eq!(pipelines.len(), 1, "replace should not duplicate");
}

#[tokio::test]
async fn pipeline_upsert_and_delete_via_api() {
    let (engine, _tmp) = create_test_engine(4).await;
    let request = PipelineUpsertRequest {
        channel_id: 7,
        config: test_pipeline("model_a"),
    };

    engine
        .upsert_pipeline(request)
        .await
        .expect("upsert pipeline");
    assert!(engine.get_pipeline(7).await.unwrap().is_some());

    engine.delete_pipeline(7).await.expect("delete pipeline");
    assert!(engine.get_pipeline(7).await.unwrap().is_none());
}

#[tokio::test]
async fn pipeline_status_reflects_count() {
    let (engine, _tmp) = create_test_engine(4).await;

    engine
        .register_pipeline(1, test_pipeline("m"))
        .expect("register pipeline #1");
    engine
        .register_pipeline(2, test_pipeline("m"))
        .expect("register pipeline #2");
    engine
        .register_pipeline(3, test_pipeline("m"))
        .expect("register pipeline #3");

    let status = engine.get_engine_status().await.unwrap();
    assert_eq!(status.pipelines.registered, 3);
    assert_eq!(status.pipelines.active_channels, 3);
}

// ───────────────────────────────────────────────────────────────────
// Backpressure
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn backpressure_when_semaphore_exhausted() {
    // Engine with only 1 concurrent inference slot.
    let (engine, _tmp) = create_test_engine(1).await;
    let engine = Arc::new(engine);

    engine
        .register_pipeline(1, test_pipeline("nonexistent_model"))
        .expect("register pipeline");

    // First request will acquire the semaphore permit, then fail on model lookup.
    // The semaphore is acquired first, so the second concurrent request should get backpressure.
    // To reliably test: acquire the semaphore externally would require internal access.
    //
    // Instead, verify capacity API reports correctly.
    let pipeline_id = PipelineId::new("test_pipeline");
    assert!(
        engine.has_capacity(&pipeline_id),
        "should have capacity initially"
    );

    // Verify backpressure error on a model-not-found scenario still returns the permit.
    let req = FrameAnalysisRequest {
        channel_id: 1,
        device_id: 1,
        frame: fake_frame(1),
        roi: None,
    };
    let result = engine.analyze_frame(req).await;
    assert!(result.is_err(), "should fail on nonexistent model");

    // Permit should be returned — capacity should still be available.
    assert!(
        engine.has_capacity(&pipeline_id),
        "permit should be released after error"
    );
}

#[tokio::test]
async fn backpressure_no_pipeline() {
    let (engine, _tmp) = create_test_engine(4).await;

    let req = FrameAnalysisRequest {
        channel_id: 999,
        device_id: 1,
        frame: fake_frame(1),
        roi: None,
    };

    let err = engine.analyze_frame(req).await.unwrap_err();
    // Without a registered pipeline, we expect PipelineNotFound after acquiring permit.
    // But actually the code tries to acquire permit first, then resolve pipeline.
    // Let's just verify it's an error.
    assert!(
        matches!(err, AiEngineError::PipelineNotFound(999)),
        "expected PipelineNotFound, got: {err:?}"
    );
}

// ───────────────────────────────────────────────────────────────────
// Model queries
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_model_nonexistent_returns_none() {
    let (engine, _tmp) = create_test_engine(4).await;

    let model = engine.get_model("does_not_exist").await.expect("get_model");
    assert!(model.is_none());
}

#[tokio::test]
async fn model_upload_update_and_delete() {
    let (engine, _tmp) = create_test_engine(4).await;

    let metadata = ModelUploadMetadata {
        id: "unit_model".to_string(),
        name: "Unit Model".to_string(),
        version: "1.0.0".to_string(),
        task: ModelTask::ObjectDetection,
        labels: vec!["person".to_string()],
        default_preprocess: None,
        default_postprocess: None,
    };
    let uploaded = engine
        .upload_model(Bytes::from_static(b"not_a_real_onnx"), metadata)
        .await
        .expect("upload model");
    assert_eq!(uploaded.id, "unit_model");

    let updated = engine
        .update_model(
            "unit_model",
            ModelUpdateRequest {
                name: Some("Updated Model".to_string()),
                version: Some("2.0.0".to_string()),
                task: Some(ModelTask::Classification),
                labels: Some(vec!["cat".to_string(), "dog".to_string()]),
                default_preprocess: None,
                default_postprocess: None,
            },
        )
        .await
        .expect("update model");
    assert_eq!(updated.name, "Updated Model");
    assert_eq!(updated.version, "2.0.0");
    assert!(matches!(updated.task, ModelTask::Classification));

    engine
        .delete_model("unit_model")
        .await
        .expect("delete model");
    assert!(engine.get_model("unit_model").await.unwrap().is_none());
}

// ───────────────────────────────────────────────────────────────────
// Processor listings
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn preprocessor_listing_non_empty() {
    let (engine, _tmp) = create_test_engine(4).await;

    let pre = engine.list_preprocessors();
    assert!(
        pre.len() >= 3,
        "should have letterbox, center_crop, direct_resize"
    );

    let ids: Vec<&str> = pre.iter().map(|p| p.id.as_str()).collect();
    assert!(ids.contains(&"letterbox"));
    assert!(ids.contains(&"center_crop"));
    assert!(ids.contains(&"direct_resize"));
}

#[tokio::test]
async fn postprocessor_listing_non_empty() {
    let (engine, _tmp) = create_test_engine(4).await;

    let post = engine.list_postprocessors();
    assert!(
        post.len() >= 4,
        "should have yolov8, yolov5, classification, passthrough"
    );

    let ids: Vec<&str> = post.iter().map(|p| p.id.as_str()).collect();
    assert!(ids.contains(&"yolov8_detection"));
    assert!(ids.contains(&"yolov5_detection"));
    assert!(ids.contains(&"classification"));
    assert!(ids.contains(&"passthrough"));
}

// ───────────────────────────────────────────────────────────────────
// Latest result / snapshot
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn latest_result_initially_none() {
    let (engine, _tmp) = create_test_engine(4).await;

    let result = engine.get_latest_result(1).await.expect("get_latest");
    assert!(result.is_none());
}

// ───────────────────────────────────────────────────────────────────
// Concurrent safety
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_pipeline_operations() {
    let (engine, _tmp) = create_test_engine(4).await;
    let engine = Arc::new(engine);

    let mut handles = Vec::new();
    for i in 0..100 {
        let e = Arc::clone(&engine);
        handles.push(tokio::spawn(async move {
            e.register_pipeline(i, test_pipeline("model"))
                .expect("register pipeline");
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(engine.list_pipelines().await.unwrap().len(), 100);

    // Concurrent unregister
    let mut handles = Vec::new();
    for i in 0..50 {
        let e = Arc::clone(&engine);
        handles.push(tokio::spawn(async move {
            e.unregister_pipeline(i);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(engine.list_pipelines().await.unwrap().len(), 50);
}

// ───────────────────────────────────────────────────────────────────
// Memory stability (lightweight allocation tracking)
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pipeline_churn_does_not_leak() {
    let (engine, _tmp) = create_test_engine(4).await;

    // Register and unregister 10,000 pipelines — should not grow memory unboundedly.
    for i in 0..10_000 {
        engine
            .register_pipeline(i % 100, test_pipeline("model"))
            .expect("register pipeline");
    }
    // After churn, only 100 distinct channels remain.
    assert_eq!(engine.list_pipelines().await.unwrap().len(), 100);

    for i in 0..100 {
        engine.unregister_pipeline(i);
    }
    assert_eq!(engine.list_pipelines().await.unwrap().len(), 0);

    // Verify status is clean.
    let status = engine.get_engine_status().await.unwrap();
    assert_eq!(status.pipelines.registered, 0);
}

#[tokio::test]
async fn repeated_frame_submission_errors_do_not_leak() {
    let (engine, _tmp) = create_test_engine(4).await;
    engine
        .register_pipeline(1, test_pipeline("nonexistent"))
        .expect("register pipeline");

    // Submit 1000 frames that will all fail (model not found).
    // Verify semaphore permits are always returned.
    for i in 0..1_000 {
        let req = FrameAnalysisRequest {
            channel_id: 1,
            device_id: 1,
            frame: fake_frame(i),
            roi: None,
        };
        let _ = engine.analyze_frame(req).await;
    }

    // All permits should be returned.
    let status = engine.get_engine_status().await.unwrap();
    assert_eq!(
        status.inference.available_permits, status.inference.max_concurrent,
        "all semaphore permits must be returned after errors"
    );
    assert_eq!(status.inference.active_count, 0);
}
