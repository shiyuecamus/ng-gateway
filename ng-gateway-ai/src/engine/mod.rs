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
//!   └── InferenceRuntime    (AiInferenceRuntime — analyze_frame hot path)
//! ```
//!
//! ## Sub-modules
//!
//! - [`annotate`] — background annotation worker loop
//! - [`builtin`] — static processor registry metadata
//! - [`remap`] — ROI coordinate remapping

mod annotate;
mod builtin;
mod remap;

use crate::{
    algorithm::{host::WasmAlgorithmHost, registry::AlgorithmRegistry},
    decoded::DecodedFrame,
    frame::decode::FrameDecoderPool,
    inference::{backend::ModelBackend, onnx::OnnxBackend},
    model::{
        prober::{OnnxModelProber, ProberRegistry, RknnModelProber},
        registry::ModelRegistry,
    },
    pipeline::{
        annotator::{DefaultFrameAnnotator, FrameAnnotator},
        compiled::CompiledStage,
        context::PipelineContext,
        registry::PipelineRegistry,
        roi::crop_frame,
        tracker::TrackerRuntime,
    },
    result::alarm::evaluate_alarm_rules,
};
use annotate::AnnotateRequest;
use bytes::Bytes;
use dashmap::DashMap;
use ng_gateway_common::metrics::NGMetricsHub;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{
        AnalysisCore, AnalysisResult, Classification, Detection, EngineAlgorithmStatus,
        EngineDecoderStatus, EngineInferenceStatus, EngineModelStatus, EnginePipelineStatus,
        EngineStatus, FrameAnalysisRequest, ProcessorInfo, RenderArtifact,
    },
    entities::ai::pipeline::RegionOfInterest,
    enums::ai::AnnotationQueueOverflowStrategy,
    settings::AiEngineConfig,
    AiAlgorithmRegistry, AiEngineApi, AiInferenceRuntime, AiModelRegistry, AiPipelineRegistry,
};
use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    sync::{mpsc, Semaphore},
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
    /// Frame decoder pool (JPEG / H.264 / H.265 → RGB24).
    frame_decoder: Arc<FrameDecoderPool>,
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
    /// Engine start time (for uptime calculation).
    started_at: Instant,
}

impl AiEngine {
    /// Create and initialize a new AI engine.
    pub async fn new(
        config: AiEngineConfig,
        metrics_hub: Arc<NGMetricsHub>,
    ) -> Result<Self, AiEngineError> {
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
        ));
        let backends: Vec<Arc<dyn ModelBackend>> = vec![onnx_backend];

        // Initialize model registry
        let model_registry = Arc::new(
            ModelRegistry::new(config.models_dir.clone(), prober_registry, backends).await?,
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

        // Frame decoder
        let frame_decoder = Arc::new(FrameDecoderPool::new(config.decoder_workers)?);

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
        let annotator: Arc<dyn FrameAnnotator> = Arc::new(DefaultFrameAnnotator);
        let latest_annotated_frames: Arc<DashMap<i32, Bytes>> = Arc::new(DashMap::new());
        let annotation_worker_handle = tokio::spawn(annotate::annotation_worker_loop(
            annotate_rx,
            Arc::clone(&annotator),
            Arc::clone(&latest_annotated_frames),
            shutdown_token.child_token(),
            annotation_parallelism,
        ));

        let engine = Self {
            model_registry,
            pipeline_registry,
            algorithm_registry,
            frame_decoder,
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

        // Take the join handle in a short scope so the MutexGuard is
        // dropped before any await point during shutdown.
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

        // 3. Decode frame
        let decoded = self
            .frame_decoder
            .decode(&request.frame)
            .await
            .map_err(|e| {
                warn!(channel_id, error = %e, "frame decode failed");
                e
            })?;

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
                DecodedFrame {
                    data: decoded.data.clone(),
                    width: decoded.width,
                    height: decoded.height,
                }
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
                        let (raw_output, coord_transform, mut infer_timing) = self
                            .model_registry
                            .infer_by_key(
                                model_key,
                                &context.current_frame,
                                stage_data.preprocessor.as_ref(),
                                stage_data.input_shape.as_ref(),
                                stage_data.input_dtype,
                            )
                            .await?;

                        let postprocess_start = Instant::now();
                        let labels_vec: Vec<String> = compiled_model
                            .info
                            .labels
                            .as_ref()
                            .map(|l| l.0.clone())
                            .unwrap_or_default();
                        let post_result = stage_data.postprocessor.process(
                            &raw_output,
                            &coord_transform,
                            &labels_vec,
                        )?;
                        infer_timing.postprocess = postprocess_start.elapsed();

                        ai.add_lock_wait_ns(duration_nanos_u64_saturating(infer_timing.infer_wait));

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

        // 8. Annotation
        let estimated_core_bytes = estimate_core_payload_bytes(core.as_ref());
        ai.add_alloc_estimate(estimated_core_bytes);
        let annotation_config = Arc::clone(&binding.compiled.annotation);
        let build_request = || AnnotateRequest {
            channel_id,
            frame: Arc::clone(&decoded),
            core: Arc::clone(&core),
            config: Arc::clone(&annotation_config),
        };
        match annotation_config.queue_overflow_strategy {
            AnnotationQueueOverflowStrategy::DropNewest => {
                match self.annotate_tx.try_send(build_request()) {
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
                let timeout =
                    std::time::Duration::from_millis(annotation_config.enqueue_timeout_ms);
                match tokio::time::timeout(timeout, self.annotate_tx.send(build_request())).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        warn!(channel_id, frame_seq, "annotation queue closed");
                    }
                    Err(_) => {
                        warn!(channel_id, frame_seq, "annotation enqueue timeout");
                    }
                }
            }
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
}
