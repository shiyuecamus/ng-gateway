//! AI Processing Engine — gateway-global singleton.
//!
//! The engine is a thin facade that composes three registries and
//! an inference runtime. All camera drivers share this single instance.
//!
//! ## Architecture
//!
//! ```text
//! AiEngine (facade)
//!   ├── ModelRegistry       (AiModelRegistry + inference routing)
//!   ├── PipelineRegistry    (AiPipelineRegistry)
//!   ├── AlgorithmRegistry   (AiAlgorithmRegistry)
//!   ├── ChannelRegistry     (GStreamer frame sources per channel)
//!   └── InferenceRuntime    (AiInferenceRuntime — analyze_frame + register_channel)
//! ```
//!
//! ## Sub-modules
//!
//! - [`annotate`] — background annotation worker loop
//! - [`builtin`] — static processor registry metadata
//! - [`channel`] — per-channel GStreamer frame acquisition and inference loop
//! - [`remap`] — ROI coordinate remapping

mod annotate;
mod builtin;
pub mod channel;
mod remap;
pub mod webrtc;

#[cfg(feature = "rknn")]
use crate::inference::rknn::RknnBackend;
use crate::{
    algorithm::{host::WasmAlgorithmHost, registry::AlgorithmRegistry},
    decoded::DecodedFrame,
    frame::{memory::HardwarePlatform, platform::PlatformCapabilities},
    inference::{backend::ModelBackend, onnx::OnnxBackend},
    model::{
        prober::{OnnxModelProber, ProberRegistry, RknnModelProber},
        registry::ModelRegistry,
    },
    pipeline::{
        annotator::{self, DefaultFrameAnnotator, FrameAnnotator},
        compiled::CompiledStage,
        context::PipelineContext,
        preprocess::PreprocessInput,
        registry::PipelineRegistry,
        roi::crop_frame,
        tracker::TrackerRuntime,
    },
    result::{alarm::evaluate_alarm_rules, trajectory::TrajectoryCache},
};
use annotate::AnnotateRequest;
use bytes::Bytes;
use channel::{ChannelFrameProcessor, ChannelRuntime};
use dashmap::DashMap;
use ng_gateway_common::metrics::NGMetricsHub;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{
        AlarmEvent, AnalysisCore, AnalysisResult, ChannelRegistration, Classification, Detection,
        EngineAlgorithmStatus, EngineDecoderStatus, EngineInferenceStatus, EngineModelStatus,
        EnginePipelineStatus, EngineStatus, FrameAnalysisRequest, ProcessorInfo, RenderArtifact,
        VideoFrame, WebRtcSignaling,
    },
    entities::{
        ai::pipeline::RegionOfInterest,
        prelude::{AlarmEventActiveModel, AnnotationConfig},
    },
    enums::ai::{AlarmEventStatus, AlarmType, AnnotationQueueOverflowStrategy, FrameFormat},
    settings::AiEngineConfig,
    AiAlgorithmRegistry, AiEngineApi, AiInferenceRuntime, AiModelRegistry, AiPipelineRegistry,
};
use ng_gateway_repository::AlarmEventRepository;
use sea_orm::{ActiveValue, DatabaseConnection};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    sync::{broadcast::Receiver, mpsc, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Shared latest-result snapshot cached per channel.
#[derive(Debug, Clone)]
struct CachedLatestResult {
    core: Arc<AnalysisCore>,
    render: RenderArtifact,
    frame_timestamp: chrono::DateTime<chrono::Utc>,
    inference_latency: std::time::Duration,
}

/// The AI Processing Engine — gateway-global facade.
///
/// Composes registries and runtime. All methods are `&self` and
/// internally synchronized for concurrent driver access.
pub struct AiEngine {
    /// Model registry (probe, install, load/unload, query, inference routing).
    model_registry: Arc<ModelRegistry>,
    /// Pipeline registry (definitions, bindings, compiled runtime).
    pipeline_registry: Arc<PipelineRegistry>,
    /// Algorithm registry (WASM lifecycle and execution).
    algorithm_registry: Arc<AlgorithmRegistry>,
    /// Registered GStreamer channel runtimes (channel_id → runtime).
    channel_registry: Arc<parking_lot::Mutex<HashMap<i32, ChannelRuntime>>>,
    /// Async annotation ingress queue (bounded, lossy when full).
    annotate_tx: mpsc::Sender<AnnotateRequest>,
    /// Annotation worker task handle for graceful shutdown.
    annotation_worker_handle: Arc<parking_lot::Mutex<Option<JoinHandle<()>>>>,
    /// Cooperative shutdown token shared by internal background workers.
    shutdown_token: CancellationToken,
    /// Guard to keep shutdown idempotent.
    shutting_down: AtomicBool,
    /// Latest rendered frame per channel (eventually consistent).
    latest_annotated_frames: Arc<DashMap<i32, Bytes>>,
    /// Global inference semaphore (bounds total concurrent inferences).
    inference_semaphore: Arc<Semaphore>,
    /// Engine configuration.
    config: Arc<AiEngineConfig>,
    /// Centralised metrics hub.
    metrics_hub: Arc<NGMetricsHub>,
    /// Latest analysis result per channel.
    latest_results: Arc<DashMap<i32, Arc<CachedLatestResult>>>,
    /// WebRTC live preview publisher registry.
    webrtc_registry: webrtc::WebRtcRegistry,
    /// Per-channel trajectory caches for trajectory-based alarm evaluation.
    trajectory_caches: Arc<DashMap<i32, TrajectoryCache>>,
    /// Engine start time (for uptime calculation).
    started_at: Instant,
}

impl AiEngine {
    /// Create and initialize a new AI engine.
    ///
    /// `db_conn` provides an externally-owned database connection for
    /// model registry initialization during gateway startup (before
    /// `NGAppContext` is set). Pass `None` only if `NGAppContext` is
    /// already initialized.
    pub async fn new(
        config: AiEngineConfig,
        metrics_hub: Arc<NGMetricsHub>,
        db_conn: Option<&DatabaseConnection>,
    ) -> Result<Self, AiEngineError> {
        // Initialize GStreamer and probe platform hardware capabilities.
        // This is idempotent and safe to call early — it caches the result
        // for subsequent `register_channel` calls.
        channel::ensure_gstreamer_init()?;

        let models_dir_str = config.models_dir.to_string_lossy().to_string();
        let algorithms_dir_str = config.algorithms_dir.to_string_lossy().to_string();
        info!(
            models_dir = %models_dir_str,
            algorithms_dir = %algorithms_dir_str,
            max_concurrent = config.max_concurrent_inferences,
            decoder_workers = config.decoder_workers,
            annotate_queue_capacity = config.annotate_queue_capacity,
            execution_provider = %config.inference.execution_provider,
            "initializing AI Processing Engine"
        );

        // Build prober registry
        let prober_registry = Arc::new(ProberRegistry::new(vec![
            Box::new(OnnxModelProber::new()),
            Box::new(RknnModelProber),
        ]));

        // Build inference backends
        let onnx_backend: Arc<dyn ModelBackend> = Arc::new(OnnxBackend::new(
            config.inference.intra_op_threads,
            config.inference.sessions_per_model,
            config.inference.request_queue_capacity,
            &config.inference.execution_provider,
        ));
        #[allow(unused_mut)]
        let mut backends: Vec<Arc<dyn ModelBackend>> = vec![onnx_backend];

        #[cfg(feature = "rknn")]
        {
            let rknn_backend: Arc<dyn ModelBackend> =
                Arc::new(RknnBackend::new(config.inference.rknn_core_mask));
            backends.push(rknn_backend);
            info!("RKNN NPU backend registered");
        }

        // Initialize model registry (with optional batching)
        let batching_config = if config.inference.batching.enabled {
            Some(config.inference.batching.clone())
        } else {
            None
        };
        let model_registry = Arc::new(
            ModelRegistry::new(
                config.models_dir.clone(),
                prober_registry,
                backends,
                batching_config,
                db_conn,
            )
            .await?,
        );
        info!(
            model_count = model_registry.len(),
            "model registry initialized"
        );

        // Initialize WASM algorithm host + registry
        let wasm_host = Arc::new(
            WasmAlgorithmHost::new(
                &config.algorithms_dir,
                config.wasm.fuel_limit,
                config.wasm.memory_limit,
            )
            .await?,
        );
        let algorithm_registry = Arc::new(AlgorithmRegistry::new(Arc::clone(&wasm_host)).await?);

        // Initialize pipeline registry
        let pipeline_registry = Arc::new(PipelineRegistry::new(Arc::clone(&model_registry)).await?);

        // Annotation worker
        let inference_semaphore = Arc::new(Semaphore::new(config.max_concurrent_inferences));
        let shutdown_token = CancellationToken::new();
        let annotation_parallelism = config
            .max_concurrent_inferences
            .max(1)
            .min(config.annotate_queue_capacity.max(1))
            .min(8);
        let annotate_queue_capacity = config.annotate_queue_capacity.max(1);
        let (annotate_tx, annotate_rx) = mpsc::channel(annotate_queue_capacity);
        // Initialize hardware JPEG encoder probe for annotation snapshots.
        let platform_caps_for_jpeg = channel::platform_capabilities()
            .cloned()
            .unwrap_or(PlatformCapabilities::probe(HardwarePlatform::Generic));
        annotator::init_hw_jpeg_encoder(platform_caps_for_jpeg.hw_jpeg_encoder.as_deref());

        let annotator: Arc<dyn FrameAnnotator> = Arc::new(DefaultFrameAnnotator);
        let latest_annotated_frames: Arc<DashMap<i32, Bytes>> = Arc::new(DashMap::new());
        let annotation_worker_handle = tokio::spawn(annotate::annotation_worker_loop(
            annotate_rx,
            Arc::clone(&annotator),
            Arc::clone(&latest_annotated_frames),
            shutdown_token.child_token(),
            annotation_parallelism,
        ));

        // Initialize WebRTC registry for live preview.
        let platform_caps = channel::platform_capabilities()
            .cloned()
            .unwrap_or_else(|| PlatformCapabilities::probe(HardwarePlatform::Generic));
        let webrtc_registry = webrtc::WebRtcRegistry::new(
            config.webrtc.clone(),
            platform_caps,
            shutdown_token.child_token(),
        );

        let trajectory_caches: Arc<DashMap<i32, TrajectoryCache>> = Arc::new(DashMap::new());

        let engine = Self {
            model_registry,
            pipeline_registry,
            algorithm_registry,
            channel_registry: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            annotate_tx,
            annotation_worker_handle: Arc::new(parking_lot::Mutex::new(Some(
                annotation_worker_handle,
            ))),
            shutdown_token,
            shutting_down: AtomicBool::new(false),
            latest_annotated_frames,
            inference_semaphore,
            config: Arc::new(config),
            metrics_hub,
            latest_results: Arc::new(DashMap::new()),
            webrtc_registry,
            trajectory_caches,
            started_at: Instant::now(),
        };

        Ok(engine)
    }

    /// Get a reference to the model registry.
    pub fn model_registry(&self) -> &Arc<ModelRegistry> {
        &self.model_registry
    }

    /// Get a reference to the pipeline registry.
    pub fn pipeline_registry(&self) -> &Arc<PipelineRegistry> {
        &self.pipeline_registry
    }

    /// Get a reference to the algorithm registry.
    pub fn algorithm_registry(&self) -> &Arc<AlgorithmRegistry> {
        &self.algorithm_registry
    }

    /// Get a reference to the centralised metrics hub.
    pub fn metrics_hub(&self) -> &Arc<NGMetricsHub> {
        &self.metrics_hub
    }

    /// Get a reference to the WebRTC live preview registry.
    pub fn webrtc_registry(&self) -> &webrtc::WebRtcRegistry {
        &self.webrtc_registry
    }

    /// Gracefully shut down the AI engine.
    ///
    /// Signals cancellation and awaits the background worker.
    /// After this call, the engine will reject new `analyze_frame` requests
    /// with backpressure errors (semaphore closed).
    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            debug!("AI engine shutdown already in progress");
            return;
        }

        info!("shutting down AI engine");

        // Drain all registered channel runtimes.
        let channels: Vec<ChannelRuntime> = {
            let mut registry = self.channel_registry.lock();
            registry.drain().map(|(_, rt)| rt).collect()
        };
        for ch in channels {
            ch.shutdown().await;
        }

        // Take the annotation worker handle in a short scope so the
        // MutexGuard is dropped before any await point during shutdown.
        let worker_handle = { self.annotation_worker_handle.lock().take() };

        if let Some(handle) = worker_handle {
            // Signal cooperative cancellation first, then reject new inferences.
            self.shutdown_token.cancel();
            self.inference_semaphore.close();

            match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
                Ok(Ok(())) => info!("annotation worker shut down cleanly"),
                Ok(Err(e)) => warn!(error = %e, "annotation worker panicked during shutdown"),
                Err(_) => warn!("annotation worker shutdown timed out"),
            }
        }

        info!(
            uptime_secs = self.started_at.elapsed().as_secs(),
            "AI engine shut down"
        );
    }
}

// ── AiEngineApi facade implementation ─────────────────────────

impl AiEngineApi for AiEngine {
    fn models(&self) -> &dyn AiModelRegistry {
        self.model_registry.as_ref()
    }

    fn pipelines(&self) -> &dyn AiPipelineRegistry {
        self.pipeline_registry.as_ref()
    }

    fn algorithms(&self) -> &dyn AiAlgorithmRegistry {
        self.algorithm_registry.as_ref()
    }

    fn runtime(&self) -> &dyn AiInferenceRuntime {
        self
    }
}

// ── ROI helpers ──────────────────────────────────────────────

fn resolve_effective_rois<'a>(
    request_roi: Option<RegionOfInterest>,
    pipeline_roi: Option<RegionOfInterest>,
    pipeline_roi_regions: &'a [RegionOfInterest],
) -> Result<Cow<'a, [RegionOfInterest]>, AiEngineError> {
    let rois = if let Some(roi) = request_roi {
        Cow::Owned(vec![roi])
    } else if !pipeline_roi_regions.is_empty() {
        Cow::Borrowed(pipeline_roi_regions)
    } else if let Some(roi) = pipeline_roi {
        Cow::Owned(vec![roi])
    } else {
        Cow::Owned(vec![RegionOfInterest::FULL])
    };

    for (idx, roi) in rois.iter().enumerate() {
        if !roi.is_valid() {
            return Err(AiEngineError::PipelineConfigError(format!(
                "invalid ROI at index {idx}: expected normalized [0,1] bounds with min < max"
            )));
        }
    }
    Ok(rois)
}

#[inline]
fn is_full_frame_roi(roi: &RegionOfInterest) -> bool {
    const EPS: f32 = 1e-6;
    (roi.x_min - 0.0).abs() < EPS
        && (roi.y_min - 0.0).abs() < EPS
        && (roi.x_max - 1.0).abs() < EPS
        && (roi.y_max - 1.0).abs() < EPS
}

// ── Annotation submission ──────────────────────────────────

/// Submit an annotation request with the configured overflow strategy.
///
/// Extracted to avoid duplicating the strategy-dispatch logic in both
/// the legacy `analyze_frame` path and the channel-mode `process_frame` path.
async fn submit_annotation<F: FnOnce() -> AnnotateRequest>(
    annotate_tx: &mpsc::Sender<AnnotateRequest>,
    config: &AnnotationConfig,
    build_request: F,
    channel_id: i32,
    frame_seq: u64,
) {
    match config.queue_overflow_strategy {
        AnnotationQueueOverflowStrategy::DropNewest => {
            match annotate_tx.try_send(build_request()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        channel_id,
                        frame_seq, "annotation queue full, dropping newest"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!(channel_id, frame_seq, "annotation queue closed");
                }
            }
        }
        AnnotationQueueOverflowStrategy::WaitForSlot => {
            let timeout = std::time::Duration::from_millis(config.enqueue_timeout_ms);
            match tokio::time::timeout(timeout, annotate_tx.send(build_request())).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => warn!(channel_id, frame_seq, "annotation queue closed"),
                Err(_) => warn!(channel_id, frame_seq, "annotation enqueue timeout"),
            }
        }
    }
}

// ── Metrics helpers ─────────────────────────────────────────

#[inline]
fn duration_nanos_u64_saturating(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[inline]
fn estimate_vec_payload_bytes<T>(values: &[T]) -> u64 {
    let bytes = values.len().saturating_mul(std::mem::size_of::<T>());
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

fn estimate_core_payload_bytes(core: &AnalysisCore) -> u64 {
    estimate_vec_payload_bytes(&core.detections)
        .saturating_add(estimate_vec_payload_bytes(&core.classifications))
        .saturating_add(estimate_vec_payload_bytes(&core.keypoint_detections))
        .saturating_add(estimate_vec_payload_bytes(&core.segmentation_masks))
        .saturating_add(estimate_vec_payload_bytes(&core.anomaly_maps))
        .saturating_add(estimate_vec_payload_bytes(&core.alarms))
}

fn estimate_render_payload_bytes(render: &RenderArtifact) -> u64 {
    render
        .annotated_frame
        .as_ref()
        .map(|jpeg| u64::try_from(jpeg.len()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn build_analysis_result(
    core: &AnalysisCore,
    frame_timestamp: chrono::DateTime<chrono::Utc>,
    inference_latency: std::time::Duration,
    render: &RenderArtifact,
) -> AnalysisResult {
    AnalysisResult {
        frame_seq: core.frame_seq,
        frame_timestamp,
        detections: Arc::clone(&core.detections),
        classifications: Arc::clone(&core.classifications),
        keypoint_detections: Arc::clone(&core.keypoint_detections),
        segmentation_masks: Arc::clone(&core.segmentation_masks),
        anomaly_maps: Arc::clone(&core.anomaly_maps),
        alarms: Arc::clone(&core.alarms),
        inference_latency,
        annotated_frame: render.annotated_frame.clone(),
    }
}

// ── Legacy frame decode (JPEG / RGB24 only) ───────────────────

/// Decode a `VideoFrame` to `DecodedFrame` for the legacy push-based API.
///
/// Only supports JPEG and RGB24 — these are the formats expected from
/// HTTP API image uploads. For H.264/H.265 video streams, use
/// `register_channel()` which handles hardware-accelerated decoding
/// via GStreamer.
fn decode_video_frame_legacy(frame: &VideoFrame) -> Result<DecodedFrame, AiEngineError> {
    match frame.format {
        FrameFormat::Rgb24 => Ok(DecodedFrame::from_rgb24(
            frame.data.clone(),
            frame.width,
            frame.height,
        )),
        FrameFormat::Jpeg => {
            let img = image::ImageReader::new(std::io::Cursor::new(frame.data.as_ref()))
                .with_guessed_format()
                .map_err(|e| AiEngineError::DecodeError(format!("format guess: {e}")))?
                .decode()
                .map_err(|e| AiEngineError::DecodeError(format!("JPEG decode: {e}")))?;
            let rgb = img.to_rgb8();
            let (w, h) = (rgb.width(), rgb.height());
            Ok(DecodedFrame::from_rgb24(
                bytes::Bytes::from(rgb.into_raw()),
                w,
                h,
            ))
        }
        other => Err(AiEngineError::DecodeError(format!(
            "legacy analyze_frame does not support {other:?} — use register_channel() \
             for H.264/H.265 streams (GStreamer hardware decoding)"
        ))),
    }
}

// ── AiInferenceRuntime implementation (hot path) ──────────────

#[async_trait::async_trait]
impl AiInferenceRuntime for AiEngine {
    async fn analyze_frame(
        &self,
        request: FrameAnalysisRequest,
    ) -> Result<AnalysisResult, AiEngineError> {
        let start = Instant::now();
        let channel_id = request.channel_id;
        let frame_seq = request.frame.seq;
        let ai = self.metrics_hub.ai();
        ai.inc_frames_submitted(&channel_id.to_string());

        // 1. Backpressure
        let _permit = self.inference_semaphore.try_acquire().map_err(|_| {
            debug!(channel_id, "frame dropped — inference queue at capacity");
            ai.inc_frames_dropped(&channel_id.to_string());
            AiEngineError::Backpressure
        })?;
        ai.inc_active_inferences();
        let _guard = scopeguard::guard((), |_| {
            self.metrics_hub.ai().dec_active_inferences();
        });

        // 2. Resolve pipeline from registry
        let binding = self
            .pipeline_registry
            .get_active_binding(channel_id)
            .ok_or_else(|| {
                warn!(channel_id, "frame rejected — no pipeline bound to channel");
                AiEngineError::PipelineNotFound(channel_id)
            })?;

        // 3. Decode frame (legacy path — JPEG/RGB24 only)
        //
        // For continuous video streams, use `register_channel()` instead.
        // The GStreamer pipeline handles hardware-accelerated decoding
        // with zero-copy DMA-buf delivery.
        let decoded = decode_video_frame_legacy(&request.frame)?;

        // 4. ROI processing
        let request_roi = request.roi.map(|bbox| RegionOfInterest {
            x_min: bbox.x_min,
            y_min: bbox.y_min,
            x_max: bbox.x_max,
            y_max: bbox.y_max,
        });
        let rois = resolve_effective_rois(
            request_roi,
            binding.compiled.roi,
            binding.compiled.roi_regions.as_ref(),
        )?;
        let decoded = Arc::new(decoded);
        let mut merged = PipelineContext::new_merge_only();

        for roi in rois.iter() {
            let roi_frame = if is_full_frame_roi(roi) {
                decoded.as_ref().try_clone()?
            } else {
                crop_frame(&decoded, roi)?
            };

            // 5. Execute pipeline stages
            let mut context = PipelineContext::new(roi_frame);
            for stage in binding.compiled.stages.iter() {
                match stage {
                    CompiledStage::Inference(stage_data) => {
                        let compiled_model = binding.compiled.model(stage_data.model).ok_or(
                            AiEngineError::InternalError(format!(
                                "compiled model handle {} not found",
                                stage_data.model.0
                            )),
                        )?;
                        let model_key = compiled_model.info.model_key.as_str();

                        // Preprocess (Engine-side, before backend dispatch)
                        let preprocess_input = PreprocessInput {
                            frame: &context.current_frame,
                            model_input_shape: stage_data.input_shape.as_ref(),
                            model_input_dtype: stage_data.input_dtype,
                        };
                        let preprocess_start = Instant::now();
                        let preprocess_output =
                            stage_data.preprocessor.process(preprocess_input)?;
                        let preprocess_elapsed = preprocess_start.elapsed();
                        let coord_transform = *preprocess_output.coord_transform();

                        // Inference (backend only does inference, no preprocessing)
                        let (raw_output, infer_timing) = self
                            .model_registry
                            .infer_by_key(model_key, preprocess_output)
                            .await?;

                        let postprocess_start = Instant::now();
                        let post_result = stage_data.postprocessor.process(
                            &raw_output,
                            &coord_transform,
                            compiled_model.class_labels.as_ref(),
                        )?;
                        let postprocess_elapsed = postprocess_start.elapsed();

                        ai.add_lock_wait_ns(duration_nanos_u64_saturating(infer_timing.infer_wait));
                        let _ = (preprocess_elapsed, postprocess_elapsed);

                        context.add_detections(post_result.detections);
                        context.add_classifications(post_result.classifications);
                        context.add_keypoint_detections(post_result.keypoint_detections);
                        context.add_segmentation_masks(post_result.segmentation_masks);
                        context.add_anomaly_maps(post_result.anomaly_maps);
                        context.custom_outputs.extend(post_result.custom_outputs);
                    }

                    CompiledStage::Tracker {
                        stage: _,
                        algorithm,
                        max_age,
                    } => {
                        let mut runtime = self
                            .pipeline_registry
                            .tracker_runtimes
                            .entry(channel_id)
                            .or_insert_with(|| TrackerRuntime::new(algorithm.clone(), *max_age));

                        if !runtime.is_compatible(algorithm, *max_age) {
                            *runtime = TrackerRuntime::new(algorithm.clone(), *max_age);
                        }

                        runtime
                            .value_mut()
                            .update(&mut context.detections, &mut context.keypoint_detections);
                    }

                    CompiledStage::FrameTransform {
                        stage: _,
                        module_id,
                        config,
                    } => {
                        let transformed = self
                            .algorithm_registry
                            .host()
                            .execute_frame_transform(
                                module_id.as_ref(),
                                &context.current_frame,
                                Arc::clone(config),
                            )
                            .await
                            .map_err(|e| {
                                warn!(channel_id, module_id = module_id.as_ref(), error = %e, "WASM frame transform failed");
                                e
                            })?;
                        context.current_frame = transformed;
                    }

                    CompiledStage::ResultProcessor {
                        stage: _,
                        module_id,
                        config,
                    } => {
                        let output = self
                            .algorithm_registry
                            .host()
                            .execute_result_processor(
                                module_id.as_ref(),
                                &context.detections,
                                &context.classifications,
                                context.current_frame.width,
                                context.current_frame.height,
                                Arc::clone(config),
                            )
                            .await
                            .map_err(|e| {
                                warn!(channel_id, module_id = module_id.as_ref(), error = %e, "WASM result processor failed");
                                e
                            })?;

                        context.detections =
                            output.detections.iter().map(Detection::from).collect();
                        context.classifications = output
                            .classifications
                            .iter()
                            .map(|c| Classification {
                                top_k: c
                                    .top_k
                                    .iter()
                                    .map(|(l, s)| (Arc::from(l.as_str()), *s))
                                    .collect(),
                            })
                            .collect();
                        context.custom_outputs.extend(output.custom_outputs);
                    }
                }
            }

            if !is_full_frame_roi(roi) {
                remap::remap_context_to_full_frame(
                    &mut context,
                    roi,
                    decoded.width,
                    decoded.height,
                );
            }

            merged.add_detections(context.detections);
            merged.add_classifications(context.classifications);
            merged.add_keypoint_detections(context.keypoint_detections);
            merged.add_segmentation_masks(context.segmentation_masks);
            merged.add_anomaly_maps(context.anomaly_maps);
            merged.custom_outputs.extend(context.custom_outputs);
        }

        // 6. Alarm evaluation
        let alarms = evaluate_alarm_rules(binding.compiled.alarm_rules.as_ref(), &merged);

        // 6b. Persist alarms to DB (fire-and-forget)
        persist_alarms(&alarms, channel_id, Some(binding.info.id));

        // 7. Metrics
        let latency = start.elapsed();
        for stage in binding.compiled.stages.iter() {
            if let CompiledStage::Inference(compiled_stage) = stage {
                if let Some(model) = binding.compiled.model(compiled_stage.model) {
                    ai.record_inference(latency, model.info.model_key.as_str());
                }
            }
        }
        for det in &merged.detections {
            ai.inc_detections(&det.class);
        }
        for alarm in &alarms {
            let severity_str = format!("{:?}", alarm.severity);
            ai.inc_alarms_triggered(alarm.alarm_type.as_ref(), &severity_str);
        }

        let core = Arc::new(AnalysisCore {
            frame_seq,
            detections: merged.detections.into(),
            classifications: merged.classifications.into(),
            keypoint_detections: merged.keypoint_detections.into(),
            segmentation_masks: merged.segmentation_masks.into(),
            anomaly_maps: merged.anomaly_maps.into(),
            alarms: alarms.into(),
        });

        // 8. Annotation (conditional: skip when disabled and no alarms)
        let estimated_core_bytes = estimate_core_payload_bytes(core.as_ref());
        ai.add_alloc_estimate(estimated_core_bytes);
        let annotation_config = Arc::clone(&binding.compiled.annotation);
        let should_annotate = annotation_config.enabled || !core.alarms.is_empty();
        if should_annotate {
            let build_request = || AnnotateRequest {
                channel_id,
                frame: Arc::clone(&decoded),
                core: Arc::clone(&core),
                config: Arc::clone(&annotation_config),
            };
            submit_annotation(
                &self.annotate_tx,
                &annotation_config,
                build_request,
                channel_id,
                frame_seq,
            )
            .await;
        }

        let render = RenderArtifact {
            annotated_frame: self
                .latest_annotated_frames
                .get(&channel_id)
                .map(|jpeg| jpeg.value().clone()),
        };

        // 9. Build result
        let frame_timestamp = request.frame.timestamp;
        let result = build_analysis_result(core.as_ref(), frame_timestamp, latency, &render);

        ai.add_copy_bytes(
            estimate_core_payload_bytes(core.as_ref())
                .saturating_add(estimate_render_payload_bytes(&render)),
        );
        self.latest_results.insert(
            channel_id,
            Arc::new(CachedLatestResult {
                core,
                render,
                frame_timestamp,
                inference_latency: latency,
            }),
        );

        debug!(
            channel_id,
            frame_seq,
            detections = result.detections.len(),
            classifications = result.classifications.len(),
            alarms = result.alarms.len(),
            latency_ms = latency.as_millis() as u64,
            "frame analysis complete"
        );

        Ok(result)
    }

    async fn register_channel(&self, request: ChannelRegistration) -> Result<(), AiEngineError> {
        let channel_id = request.channel_id;

        // Ensure GStreamer is initialized (idempotent).
        channel::ensure_gstreamer_init()?;

        // Check for duplicate registration.
        {
            let registry = self.channel_registry.lock();
            if registry.contains_key(&channel_id) {
                return Err(AiEngineError::InternalError(format!(
                    "channel {channel_id} is already registered"
                )));
            }
        }

        // Resolve sampling strategy from the pipeline definition.
        let sampling = self
            .pipeline_registry
            .get_active_binding(channel_id)
            .map(|b| b.compiled.sampling.clone())
            .unwrap_or_default();

        // Build a processor that delegates to this engine's pipeline logic.
        let processor: Arc<dyn ChannelFrameProcessor> = Arc::new(EngineFrameProcessor {
            model_registry: Arc::clone(&self.model_registry),
            pipeline_registry: Arc::clone(&self.pipeline_registry),
            algorithm_registry: Arc::clone(&self.algorithm_registry),
            annotate_tx: self.annotate_tx.clone(),
            inference_semaphore: Arc::clone(&self.inference_semaphore),
            metrics_hub: Arc::clone(&self.metrics_hub),
            latest_results: Arc::clone(&self.latest_results),
            latest_annotated_frames: Arc::clone(&self.latest_annotated_frames),
            trajectory_caches: Arc::clone(&self.trajectory_caches),
        });

        let runtime = ChannelRuntime::spawn(
            &request,
            sampling,
            processor,
            Some(Arc::new(self.webrtc_registry.clone())),
            self.shutdown_token.clone(),
        )?;

        info!(
            channel_id,
            stream_url = %request.stream_url,
            "camera channel registered for AI analysis"
        );

        self.channel_registry.lock().insert(channel_id, runtime);
        Ok(())
    }

    async fn unregister_channel(&self, channel_id: i32) -> Result<(), AiEngineError> {
        let runtime = { self.channel_registry.lock().remove(&channel_id) };

        match runtime {
            Some(rt) => {
                rt.shutdown().await;
                info!(channel_id, "camera channel unregistered");
                Ok(())
            }
            None => Err(AiEngineError::InternalError(format!(
                "channel {channel_id} is not registered"
            ))),
        }
    }

    fn has_capacity(&self, _channel_id: &i32) -> bool {
        self.inference_semaphore.available_permits() > 0
    }

    async fn get_latest_result(
        &self,
        channel_id: i32,
    ) -> Result<Option<AnalysisResult>, AiEngineError> {
        Ok(self.latest_results.get(&channel_id).map(|snapshot| {
            let snapshot = snapshot.value();
            build_analysis_result(
                snapshot.core.as_ref(),
                snapshot.frame_timestamp,
                snapshot.inference_latency,
                &snapshot.render,
            )
        }))
    }

    async fn get_engine_status(&self) -> Result<EngineStatus, AiEngineError> {
        Ok(EngineStatus {
            enabled: true,
            execution_provider: self.config.inference.execution_provider.clone(),
            models: EngineModelStatus {
                registered: self.model_registry.len(),
                loaded: self.model_registry.total_loaded_count(),
                total_memory_bytes: self.model_registry.total_estimated_memory_bytes(),
            },
            inference: EngineInferenceStatus {
                active_count: self.metrics_hub.ai().active_inference_count(),
                max_concurrent: self.config.max_concurrent_inferences,
                available_permits: self.inference_semaphore.available_permits(),
                total_inferences: self.metrics_hub.ai().total_inferences(),
                avg_latency_ms: self.metrics_hub.ai().avg_latency_ms(),
            },
            pipelines: EnginePipelineStatus {
                registered: self.pipeline_registry.definition_count(),
                active_channels: self.pipeline_registry.active_binding_count(),
            },
            algorithms: EngineAlgorithmStatus {
                registered: self.algorithm_registry.algorithm_count(),
                wasm_modules: self.algorithm_registry.algorithm_count(),
            },
            decoder: EngineDecoderStatus {
                workers: self.config.decoder_workers,
                queue_depth: 0,
            },
            uptime_secs: self.started_at.elapsed().as_secs(),
        })
    }

    fn list_preprocessors(&self) -> Vec<ProcessorInfo> {
        builtin::builtin_preprocessors()
    }

    fn list_postprocessors(&self) -> Vec<ProcessorInfo> {
        builtin::builtin_postprocessors()
    }

    async fn handle_webrtc_signaling(
        &self,
        channel_id: i32,
        msg: WebRtcSignaling,
    ) -> Result<Option<WebRtcSignaling>, AiEngineError> {
        if !self.config.webrtc.enabled {
            return Err(AiEngineError::EngineNotAvailable);
        }

        // Convert models-layer signaling type to engine-layer signaling type.
        let engine_msg = match msg {
            WebRtcSignaling::Offer { sdp, config } => webrtc::SignalingMessage::Offer {
                sdp,
                config: config.map(|c| webrtc::ClientConfig {
                    preferred_codec: c.preferred_codec,
                    max_resolution: c.max_resolution,
                    max_fps: c.max_fps,
                    server_side_annotation: c.server_side_annotation,
                }),
            },
            WebRtcSignaling::Ice {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => webrtc::SignalingMessage::Ice {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            },
            _ => {
                return Ok(Some(WebRtcSignaling::Error {
                    message: "unexpected message type from client".to_string(),
                }));
            }
        };

        // Get or create the publisher for this channel.
        // Use a default resolution — the actual resolution is set when the
        // first frame arrives from the channel's analysis pipeline.
        let handle = self.webrtc_registry.get_or_create(
            channel_id,
            self.config.webrtc.max_width,
            self.config.webrtc.max_height,
        )?;

        let response = handle.handle_signaling(engine_msg).await;

        // Convert engine-layer response back to models-layer type.
        Ok(response.map(|r| match r {
            webrtc::SignalingMessage::Answer { sdp } => WebRtcSignaling::Answer { sdp },
            webrtc::SignalingMessage::Ice {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => WebRtcSignaling::Ice {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            },
            webrtc::SignalingMessage::Connected {
                channel_id,
                video_codec,
                resolution,
                fps,
                hw_encoder,
            } => WebRtcSignaling::Connected {
                channel_id,
                video_codec,
                resolution,
                fps,
                hw_encoder,
            },
            webrtc::SignalingMessage::Error { message } => WebRtcSignaling::Error { message },
            _ => WebRtcSignaling::Error {
                message: "unexpected internal response".to_string(),
            },
        }))
    }

    fn is_webrtc_enabled(&self) -> bool {
        self.config.webrtc.enabled
    }

    fn subscribe_webrtc_server_ice(&self, channel_id: i32) -> Option<Receiver<WebRtcSignaling>> {
        if !self.config.webrtc.enabled {
            return None;
        }
        let handle = self
            .webrtc_registry
            .get_or_create(
                channel_id,
                self.config.webrtc.max_width,
                self.config.webrtc.max_height,
            )
            .ok()?;
        Some(handle.subscribe_server_ice())
    }

    fn webrtc_add_peer(&self, channel_id: i32) -> Option<()> {
        if !self.config.webrtc.enabled {
            return None;
        }
        let handle = self
            .webrtc_registry
            .get_or_create(
                channel_id,
                self.config.webrtc.max_width,
                self.config.webrtc.max_height,
            )
            .ok()?;
        handle.add_peer();
        Some(())
    }

    fn webrtc_remove_peer(&self, channel_id: i32) {
        if !self.config.webrtc.enabled {
            return;
        }
        if let Some(handle) = self.webrtc_registry.get(channel_id) {
            if handle.remove_peer() {
                self.webrtc_registry.remove(channel_id);
            }
        }
    }
}

// ── EngineFrameProcessor — ChannelFrameProcessor for GStreamer channels ──

/// Shared inference context consumed by per-channel frame loops.
///
/// Holds `Arc` clones of all engine subsystems needed to process a
/// decoded frame. Lightweight to clone — all inner fields are `Arc`.
struct EngineFrameProcessor {
    model_registry: Arc<ModelRegistry>,
    pipeline_registry: Arc<PipelineRegistry>,
    algorithm_registry: Arc<AlgorithmRegistry>,
    annotate_tx: mpsc::Sender<AnnotateRequest>,
    inference_semaphore: Arc<Semaphore>,
    metrics_hub: Arc<NGMetricsHub>,
    latest_results: Arc<DashMap<i32, Arc<CachedLatestResult>>>,
    latest_annotated_frames: Arc<DashMap<i32, Bytes>>,
    /// Per-channel trajectory caches for trajectory-based alarm evaluation.
    trajectory_caches: Arc<DashMap<i32, TrajectoryCache>>,
}

#[async_trait::async_trait]
impl ChannelFrameProcessor for EngineFrameProcessor {
    async fn process_frame(
        &self,
        channel_id: i32,
        _device_id: i32,
        frame: crate::DecodedFrame,
        frame_seq: u64,
    ) -> Result<AnalysisResult, AiEngineError> {
        let start = Instant::now();
        let ai = self.metrics_hub.ai();
        ai.inc_frames_submitted(&channel_id.to_string());

        // 1. Backpressure
        let _permit = self.inference_semaphore.try_acquire().map_err(|_| {
            debug!(channel_id, "frame dropped — inference queue at capacity");
            ai.inc_frames_dropped(&channel_id.to_string());
            AiEngineError::Backpressure
        })?;
        ai.inc_active_inferences();
        let metrics_hub = Arc::clone(&self.metrics_hub);
        let _guard = scopeguard::guard((), move |_| {
            metrics_hub.ai().dec_active_inferences();
        });

        // 2. Resolve pipeline binding
        let binding = self
            .pipeline_registry
            .get_active_binding(channel_id)
            .ok_or_else(|| {
                warn!(channel_id, "frame rejected — no pipeline bound to channel");
                AiEngineError::PipelineNotFound(channel_id)
            })?;

        // 3. ROI processing (no request-level ROI for channel mode)
        let rois = resolve_effective_rois(
            None,
            binding.compiled.roi,
            binding.compiled.roi_regions.as_ref(),
        )?;
        let decoded = Arc::new(frame);
        let mut merged = PipelineContext::new_merge_only();

        for roi in rois.iter() {
            let roi_frame = if is_full_frame_roi(roi) {
                decoded.as_ref().try_clone()?
            } else {
                crop_frame(&decoded, roi)?
            };

            // 4. Execute pipeline stages
            let mut context = PipelineContext::new(roi_frame);
            for stage in binding.compiled.stages.iter() {
                match stage {
                    CompiledStage::Inference(stage_data) => {
                        let compiled_model = binding.compiled.model(stage_data.model).ok_or(
                            AiEngineError::InternalError(format!(
                                "compiled model handle {} not found",
                                stage_data.model.0
                            )),
                        )?;
                        let model_key = compiled_model.info.model_key.as_str();

                        // Preprocess (Engine-side, before backend dispatch)
                        let preprocess_input = PreprocessInput {
                            frame: &context.current_frame,
                            model_input_shape: stage_data.input_shape.as_ref(),
                            model_input_dtype: stage_data.input_dtype,
                        };
                        let preprocess_start = Instant::now();
                        let preprocess_output =
                            stage_data.preprocessor.process(preprocess_input)?;
                        let preprocess_elapsed = preprocess_start.elapsed();
                        let coord_transform = *preprocess_output.coord_transform();

                        // Inference (backend only does inference, no preprocessing)
                        let (raw_output, infer_timing) = self
                            .model_registry
                            .infer_by_key(model_key, preprocess_output)
                            .await?;

                        let postprocess_start = Instant::now();
                        let post_result = stage_data.postprocessor.process(
                            &raw_output,
                            &coord_transform,
                            compiled_model.class_labels.as_ref(),
                        )?;
                        let postprocess_elapsed = postprocess_start.elapsed();
                        ai.add_lock_wait_ns(duration_nanos_u64_saturating(infer_timing.infer_wait));
                        let _ = (preprocess_elapsed, postprocess_elapsed);

                        context.add_detections(post_result.detections);
                        context.add_classifications(post_result.classifications);
                        context.add_keypoint_detections(post_result.keypoint_detections);
                        context.add_segmentation_masks(post_result.segmentation_masks);
                        context.add_anomaly_maps(post_result.anomaly_maps);
                        context.custom_outputs.extend(post_result.custom_outputs);
                    }

                    CompiledStage::Tracker {
                        stage: _,
                        algorithm,
                        max_age,
                    } => {
                        let mut runtime = self
                            .pipeline_registry
                            .tracker_runtimes
                            .entry(channel_id)
                            .or_insert_with(|| TrackerRuntime::new(algorithm.clone(), *max_age));

                        if !runtime.is_compatible(algorithm, *max_age) {
                            *runtime = TrackerRuntime::new(algorithm.clone(), *max_age);
                        }

                        runtime
                            .value_mut()
                            .update(&mut context.detections, &mut context.keypoint_detections);
                    }

                    CompiledStage::FrameTransform {
                        stage: _,
                        module_id,
                        config,
                    } => {
                        let transformed = self
                            .algorithm_registry
                            .host()
                            .execute_frame_transform(
                                module_id.as_ref(),
                                &context.current_frame,
                                Arc::clone(config),
                            )
                            .await
                            .map_err(|e| {
                                warn!(channel_id, module_id = module_id.as_ref(), error = %e, "WASM frame transform failed");
                                e
                            })?;
                        context.current_frame = transformed;
                    }

                    CompiledStage::ResultProcessor {
                        stage: _,
                        module_id,
                        config,
                    } => {
                        let output = self
                            .algorithm_registry
                            .host()
                            .execute_result_processor(
                                module_id.as_ref(),
                                &context.detections,
                                &context.classifications,
                                context.current_frame.width,
                                context.current_frame.height,
                                Arc::clone(config),
                            )
                            .await
                            .map_err(|e| {
                                warn!(channel_id, module_id = module_id.as_ref(), error = %e, "WASM result processor failed");
                                e
                            })?;

                        context.detections =
                            output.detections.iter().map(Detection::from).collect();
                        context.classifications = output
                            .classifications
                            .iter()
                            .map(|c| Classification {
                                top_k: c
                                    .top_k
                                    .iter()
                                    .map(|(l, s)| (Arc::from(l.as_str()), *s))
                                    .collect(),
                            })
                            .collect();
                        context.custom_outputs.extend(output.custom_outputs);
                    }
                }
            }

            if !is_full_frame_roi(roi) {
                remap::remap_context_to_full_frame(
                    &mut context,
                    roi,
                    decoded.width,
                    decoded.height,
                );
            }

            merged.add_detections(context.detections);
            merged.add_classifications(context.classifications);
            merged.add_keypoint_detections(context.keypoint_detections);
            merged.add_segmentation_masks(context.segmentation_masks);
            merged.add_anomaly_maps(context.anomaly_maps);
            merged.custom_outputs.extend(context.custom_outputs);
        }

        // 5. Alarm evaluation (two-phase: standard + trajectory-based)
        let mut alarms = evaluate_alarm_rules(binding.compiled.alarm_rules.as_ref(), &merged);

        // 5a. Trajectory-based alarm evaluation (line-crossing, zone-dwell).
        // Updates the per-channel trajectory cache and evaluates rules that
        // require temporal context across multiple frames.
        {
            let current_ts = chrono::Utc::now().timestamp_millis();
            let mut cache_entry = self
                .trajectory_caches
                .entry(channel_id)
                .or_insert_with(TrajectoryCache::default);
            let cache = cache_entry.value_mut();

            cache.update_from_detections(&merged.detections, current_ts);
            cache.evict_stale(current_ts);

            // Build the rule descriptors for trajectory evaluation.
            let traj_rules: Vec<_> = binding
                .compiled
                .alarm_rules
                .iter()
                .filter(|r| {
                    matches!(
                        r.condition,
                        ng_gateway_models::entities::ai::alarm_rule::AlarmCondition::LineCrossing { .. }
                            | ng_gateway_models::entities::ai::alarm_rule::AlarmCondition::ZoneDwell { .. }
                    )
                })
                .map(|r| {
                    let cooldown_ms = r.cooldown_secs as i64 * 1000;
                    (r.id, r.name.as_str(), r.severity, &r.condition, cooldown_ms)
                })
                .collect();

            if !traj_rules.is_empty() {
                let traj_alarms = cache.evaluate_trajectory_rules(&traj_rules, current_ts);
                alarms.extend(traj_alarms);
            }
        }

        // 5b. Persist alarms to DB (fire-and-forget)
        persist_alarms(&alarms, channel_id, Some(binding.info.id));

        // 6. Metrics
        let latency = start.elapsed();
        for stage in binding.compiled.stages.iter() {
            if let CompiledStage::Inference(compiled_stage) = stage {
                if let Some(model) = binding.compiled.model(compiled_stage.model) {
                    ai.record_inference(latency, model.info.model_key.as_str());
                }
            }
        }
        for det in &merged.detections {
            ai.inc_detections(&det.class);
        }
        for alarm in &alarms {
            let severity_str = format!("{:?}", alarm.severity);
            ai.inc_alarms_triggered(alarm.alarm_type.as_ref(), &severity_str);
        }

        let core = Arc::new(AnalysisCore {
            frame_seq,
            detections: merged.detections.into(),
            classifications: merged.classifications.into(),
            keypoint_detections: merged.keypoint_detections.into(),
            segmentation_masks: merged.segmentation_masks.into(),
            anomaly_maps: merged.anomaly_maps.into(),
            alarms: alarms.into(),
        });

        // 7. Annotation (conditional: skip when disabled and no alarms)
        let estimated_core_bytes = estimate_core_payload_bytes(core.as_ref());
        ai.add_alloc_estimate(estimated_core_bytes);
        let annotation_config = Arc::clone(&binding.compiled.annotation);
        let should_annotate = annotation_config.enabled || !core.alarms.is_empty();
        if should_annotate {
            let build_request = || AnnotateRequest {
                channel_id,
                frame: Arc::clone(&decoded),
                core: Arc::clone(&core),
                config: Arc::clone(&annotation_config),
            };
            submit_annotation(
                &self.annotate_tx,
                &annotation_config,
                build_request,
                channel_id,
                frame_seq,
            )
            .await;
        }

        let render = RenderArtifact {
            annotated_frame: self
                .latest_annotated_frames
                .get(&channel_id)
                .map(|jpeg| jpeg.value().clone()),
        };

        // 8. Build result
        let frame_timestamp = chrono::Utc::now();
        let result = build_analysis_result(core.as_ref(), frame_timestamp, latency, &render);

        ai.add_copy_bytes(
            estimate_core_payload_bytes(core.as_ref())
                .saturating_add(estimate_render_payload_bytes(&render)),
        );
        self.latest_results.insert(
            channel_id,
            Arc::new(CachedLatestResult {
                core,
                render,
                frame_timestamp,
                inference_latency: latency,
            }),
        );

        debug!(
            channel_id,
            frame_seq,
            detections = result.detections.len(),
            classifications = result.classifications.len(),
            alarms = result.alarms.len(),
            latency_ms = latency.as_millis() as u64,
            "frame analysis complete (channel mode)"
        );

        Ok(result)
    }
}

// ── Alarm event persistence ─────────────────────────────────────

/// Persist alarm events to the database in a fire-and-forget background task.
///
/// This is intentionally non-blocking: the inference hot path must never
/// wait for a database write. Persistence failures are logged but do not
/// affect the inference pipeline.
fn persist_alarms(alarms: &[AlarmEvent], channel_id: i32, pipeline_id: Option<i32>) {
    if alarms.is_empty() {
        return;
    }

    let rows: Vec<AlarmEventActiveModel> = alarms
        .iter()
        .map(|alarm| {
            let alarm_type = match alarm.alarm_type.as_ref() {
                "class_detected" => AlarmType::ClassDetected,
                "count_exceeds" => AlarmType::CountExceeds,
                "zone_intrusion" => AlarmType::ZoneIntrusion,
                "line_crossing" => AlarmType::LineCrossing,
                "anomaly_detected" => AlarmType::AnomalyDetected,
                "zone_dwell" => AlarmType::ZoneDwell,
                _ => AlarmType::CustomWasm,
            };

            // Build payload: include trajectory context if present,
            // otherwise fall back to related detections.
            let payload = if let Some(ref traj) = alarm.trajectory {
                serde_json::to_value(traj).ok()
            } else if !alarm.related_detections.is_empty() {
                serde_json::to_value(&alarm.related_detections).ok()
            } else {
                None
            };

            let now = chrono::Utc::now();
            AlarmEventActiveModel {
                id: ActiveValue::NotSet,
                channel_id: ActiveValue::Set(channel_id),
                pipeline_id: ActiveValue::Set(pipeline_id),
                alarm_type: ActiveValue::Set(alarm_type),
                severity: ActiveValue::Set(alarm.severity),
                description: ActiveValue::Set(alarm.description.to_string()),
                payload: ActiveValue::Set(payload),
                status: ActiveValue::Set(AlarmEventStatus::Open),
                acked_at: ActiveValue::Set(None),
                closed_at: ActiveValue::Set(None),
                created_at: ActiveValue::Set(now),
                updated_at: ActiveValue::Set(now),
            }
        })
        .collect();

    tokio::spawn(async move {
        for row in rows {
            if let Err(e) =
                AlarmEventRepository::create::<sea_orm::DatabaseConnection>(row, None).await
            {
                warn!(channel_id, error = %e, "failed to persist alarm event");
            }
        }
    });
}
