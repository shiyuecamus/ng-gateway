//! Memory stability test — verifies that the AI engine does not leak memory
//! under sustained workload.
//!
//! Strategy:
//! - Run a tight loop of pipeline register/unregister + frame submission (error path)
//!   for many iterations.
//! - Sample process RSS at intervals and assert it stays within a bounded range.
//! - This catches:
//!   - DashMap entry leaks
//!   - Semaphore permit leaks
//!   - Arc/Bytes reference leaks
//!   - Allocator fragmentation beyond tolerance
//!
//! # Running
//! ```bash
//! cargo test -p ng-gateway-ai --features engine --test memory_stability -- --nocapture
//! ```

#![cfg(feature = "engine")]
use bytes::Bytes;
use chrono::Utc;
use ng_gateway_ai::engine::AiEngine;
use ng_gateway_common::metrics::NGMetricsHub;
use ng_gateway_models::{
    ai::{
        api::AiEngineApi,
        pipeline::{AnnotationConfig, PipelineConfig, SamplingStrategy, StageConfig},
        types::{FrameAnalysisRequest, FrameFormat, PipelineId, VideoFrame},
    },
    settings::AiEngineConfig,
};
use std::sync::Arc;
use tempfile::TempDir;

/// Read the current process RSS (Resident Set Size) in bytes.
/// Falls back to 0 on platforms where `/proc/self/statm` is unavailable.
fn rss_bytes() -> usize {
    #[cfg(target_os = "linux")]
    {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
            .map(|pages| pages * page_size)
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // On macOS / Windows, use a simpler heuristic via sysinfo or just return 0.
        // The test will skip the RSS assertion on non-Linux but still runs the workload.
        0
    }
}

async fn create_engine() -> (AiEngine, TempDir) {
    let tmp = TempDir::new().expect("create temp dir");
    let metrics_hub = Arc::new(NGMetricsHub::new().expect("metrics hub"));
    let config = AiEngineConfig {
        enabled: true,
        models_dir: tmp.path().join("models"),
        algorithms_dir: tmp.path().join("algorithms"),
        max_concurrent_inferences: 4,
        decoder_workers: 1,
        ..Default::default()
    };
    let engine = AiEngine::new(config, metrics_hub)
        .await
        .expect("engine init");
    (engine, tmp)
}

fn make_pipeline() -> PipelineConfig {
    PipelineConfig {
        id: PipelineId::new("stress"),
        name: "Stress Pipeline".into(),
        sampling: SamplingStrategy::EveryFrame,
        roi: None,
        roi_regions: vec![],
        stages: vec![StageConfig::Inference {
            model_id: "nonexistent".into(),
            confidence_threshold: 0.5,
            nms_iou_threshold: None,
            input_size: None,
            preprocess: None,
            postprocess: None,
        }],
        alarm_rules: vec![],
        annotation: AnnotationConfig::default(),
    }
}

fn make_frame(seq: u64) -> VideoFrame {
    // Tiny synthetic JPEG-like payload (not a valid JPEG, but enough to exercise
    // the Bytes allocation/deallocation path).
    let data = vec![0xFFu8, 0xD8, 0xFF, 0xD9];
    VideoFrame {
        data: Bytes::from(data),
        format: FrameFormat::Jpeg,
        width: 1,
        height: 1,
        timestamp: Utc::now(),
        seq,
    }
}

/// Sustained pipeline churn + frame submission stress test.
///
/// Runs 50,000 iterations of register/submit/unregister and verifies:
/// 1. All semaphore permits are returned
/// 2. Pipeline map is clean
/// 3. RSS does not grow more than 50 MB above baseline (Linux only)
#[tokio::test]
async fn sustained_workload_memory_stable() {
    let (engine, _tmp) = create_engine().await;
    let engine = Arc::new(engine);

    const ITERATIONS: u64 = 50_000;
    const CHANNEL_SLOTS: i32 = 50;

    // Warm up: force initial allocations to settle.
    for i in 0..CHANNEL_SLOTS {
        engine
            .register_pipeline(i, make_pipeline())
            .expect("register warmup pipeline");
    }
    for i in 0..CHANNEL_SLOTS {
        engine.unregister_pipeline(i);
    }

    // Baseline RSS after warm-up.
    let baseline_rss = rss_bytes();

    // Stress loop
    for i in 0..ITERATIONS {
        let ch = (i % CHANNEL_SLOTS as u64) as i32;

        engine
            .register_pipeline(ch, make_pipeline())
            .expect("register stress pipeline");

        let req = FrameAnalysisRequest {
            channel_id: ch,
            device_id: 1,
            frame: make_frame(i),
            roi: None,
        };
        // This will fail (model not found) — we're exercising alloc/dealloc paths.
        let _ = engine.analyze_frame(req).await;

        // Periodically unregister to exercise DashMap remove path.
        if i % 100 == 99 {
            engine.unregister_pipeline(ch);
        }
    }

    // Clean up all pipelines
    for i in 0..CHANNEL_SLOTS {
        engine.unregister_pipeline(i);
    }

    // Verify engine state is clean.
    let status = engine.get_engine_status().await.unwrap();
    assert_eq!(
        status.pipelines.registered, 0,
        "all pipelines should be unregistered"
    );
    assert_eq!(
        status.inference.available_permits, status.inference.max_concurrent,
        "all semaphore permits should be returned"
    );
    assert_eq!(status.inference.active_count, 0, "no active inferences");

    // RSS check (Linux only).
    let final_rss = rss_bytes();
    if baseline_rss > 0 && final_rss > 0 {
        let growth = final_rss.saturating_sub(baseline_rss);
        let growth_mb = growth / (1024 * 1024);
        println!(
            "[memory] baseline={:.1}MB  final={:.1}MB  growth={:.1}MB  iterations={}",
            baseline_rss as f64 / 1048576.0,
            final_rss as f64 / 1048576.0,
            growth_mb as f64,
            ITERATIONS
        );
        // Allow up to 50 MB growth (covers allocator fragmentation, thread stacks, etc.)
        assert!(
            growth_mb < 50,
            "RSS grew by {growth_mb} MB over {ITERATIONS} iterations — possible leak"
        );
    } else {
        println!(
            "[memory] RSS measurement unavailable on this platform — \
             skipping RSS assertion (workload still executed)"
        );
    }
}
