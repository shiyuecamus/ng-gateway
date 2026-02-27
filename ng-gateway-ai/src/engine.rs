//! AI Processing Engine — gateway-global singleton.
//!
//! Manages model lifecycle, inference worker pools, and pipeline orchestration.
//! All camera drivers share this single engine instance.

#[cfg(feature = "engine")]
mod inner {
    use crate::{
        algorithm::host::WasmAlgorithmHost,
        decoded::DecodedFrame,
        frame::decode::FrameDecoderPool,
        inference::pool::InferencePool,
        model::registry::ModelRegistry,
        pipeline::{
            annotator::{DefaultFrameAnnotator, FrameAnnotator},
            compiled::{compile_pipeline, CompiledPipeline, CompiledStage},
            context::PipelineContext,
            roi::crop_frame,
            tracker::TrackerRuntime,
        },
        result::alarm::evaluate_alarm_rules,
    };
    use bytes::Bytes;
    use dashmap::DashMap;
    use ng_gateway_common::metrics::NGMetricsHub;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::{
        ai::{
            algorithm::{
                AlgorithmTestInput, AlgorithmTestResult, AlgorithmUploadMetadata, WasmAlgorithmInfo,
            },
            api::AiEngineApi,
            model::{ModelFormat, ModelInfo, ModelUpdateRequest, ModelUploadMetadata},
            pipeline::{AnnotationQueueOverflowStrategy, PipelineConfig, PipelineUpsertRequest},
            types::{
                AnalysisCore, AnalysisResult, BoundingBox, Classification, Detection,
                EngineAlgorithmStatus, EngineDecoderStatus, EngineInferenceStatus,
                EngineModelStatus, EnginePipelineStatus, EngineStatus, FrameAnalysisRequest,
                ParamType, PipelineId, ProcessorInfo, ProcessorParameter, RegionOfInterest,
                RenderArtifact, SegmentationMask,
            },
        },
        settings::AiEngineConfig,
    };
    use std::{
        borrow::Cow,
        path::{Path, PathBuf},
        sync::Arc,
        time::Instant,
    };
    use tokio::sync::{mpsc, Semaphore};
    use tracing::{debug, info, info_span, warn};

    /// Shared latest-result snapshot cached per channel.
    #[derive(Debug, Clone)]
    struct CachedLatestResult {
        /// Structured analysis core shared across readers.
        core: Arc<AnalysisCore>,
        /// Rendering payload stored independently from structured core.
        render: RenderArtifact,
        /// Source frame timestamp.
        frame_timestamp: chrono::DateTime<chrono::Utc>,
        /// End-to-end inference latency.
        inference_latency: std::time::Duration,
    }

    /// Registered pipeline payload containing user config and compiled runtime plan.
    struct RegisteredPipeline {
        /// Original user-facing pipeline config (for API query/list).
        config: Arc<PipelineConfig>,
        /// Runtime-optimized immutable compiled pipeline.
        compiled: Arc<CompiledPipeline>,
    }

    /// Async annotation request submitted from the inference hot path.
    struct AnnotateRequest {
        /// Source channel id.
        channel_id: i32,
        /// Decoded frame for rendering.
        frame: Arc<DecodedFrame>,
        /// Structured analysis result.
        core: Arc<AnalysisCore>,
        /// Per-pipeline annotation config snapshot.
        config: Arc<ng_gateway_models::ai::pipeline::AnnotationConfig>,
    }

    /// The AI Processing Engine — gateway-global singleton.
    ///
    /// Manages model lifecycle, inference worker pools, pipeline orchestration,
    /// and frame annotation. All camera drivers share this single engine instance.
    pub struct AiEngine {
        /// Model registry (hot-swappable).
        model_registry: Arc<ModelRegistry>,
        /// Inference worker pool.
        inference_pool: Arc<InferencePool>,
        /// Pipeline configurations per channel.
        pipelines: Arc<DashMap<i32, Arc<RegisteredPipeline>>>,
        /// Frame decoder pool (JPEG / H.264 / H.265 → RGB24).
        frame_decoder: Arc<FrameDecoderPool>,
        /// WASM algorithm runtime (FrameTransform + ResultProcessor execution).
        wasm_runtime: Arc<WasmAlgorithmHost>,
        /// Async annotation ingress queue (bounded, lossy when full).
        annotate_tx: mpsc::Sender<AnnotateRequest>,
        /// Latest rendered frame per channel (eventually consistent).
        latest_annotated_frames: Arc<DashMap<i32, Bytes>>,
        /// Global inference semaphore (bounds total concurrent inferences).
        inference_semaphore: Arc<Semaphore>,
        /// Engine configuration (retained for runtime reconfiguration and status API).
        config: Arc<AiEngineConfig>,
        /// Centralised metrics hub (shared with all gateway subsystems).
        metrics_hub: Arc<NGMetricsHub>,
        /// Latest analysis result per channel (for snapshot API).
        latest_results: Arc<DashMap<i32, Arc<CachedLatestResult>>>,
        /// Per-channel tracker runtimes for `StageConfig::Tracker`.
        tracker_runtimes: Arc<DashMap<i32, TrackerRuntime>>,
        /// Engine start time (for uptime calculation).
        started_at: Instant,
    }

    impl AiEngine {
        /// Create and initialize a new AI engine.
        ///
        /// The `metrics_hub` is the gateway-wide metrics hub; AI metrics are
        /// accessed via `metrics_hub.ai()` and registered into the shared
        /// Prometheus registry so they appear on `GET /metrics`.
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

            let model_registry = Arc::new(ModelRegistry::new(&config.models_dir).await?);
            let model_count = model_registry
                .list_all()
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            info!(model_count, "model registry initialized");

            let inference_pool = Arc::new(
                InferencePool::new(
                    Arc::clone(&model_registry),
                    config.inference.intra_op_threads,
                    config.inference.sessions_per_model,
                    config.inference.request_queue_capacity,
                )
                .await?,
            );

            let frame_decoder = Arc::new(FrameDecoderPool::new(config.decoder_workers)?);

            let wasm_runtime = Arc::new(
                WasmAlgorithmHost::new(
                    &config.algorithms_dir,
                    config.wasm.fuel_limit,
                    config.wasm.memory_limit,
                )
                .await?,
            );
            let wasm_count = wasm_runtime.algorithm_count();
            if wasm_count > 0 {
                info!(wasm_count, "WASM algorithm host initialized");
            }

            let inference_semaphore = Arc::new(Semaphore::new(config.max_concurrent_inferences));
            let annotate_queue_capacity = config.annotate_queue_capacity.max(1);
            let (annotate_tx, annotate_rx) = mpsc::channel(annotate_queue_capacity);
            let annotator: Arc<dyn FrameAnnotator> = Arc::new(DefaultFrameAnnotator);
            let latest_annotated_frames: Arc<DashMap<i32, Bytes>> = Arc::new(DashMap::new());
            tokio::spawn(annotation_worker_loop(
                annotate_rx,
                Arc::clone(&annotator),
                Arc::clone(&latest_annotated_frames),
            ));

            Ok(Self {
                model_registry,
                inference_pool,
                pipelines: Arc::new(DashMap::new()),
                frame_decoder,
                wasm_runtime,
                annotate_tx,
                latest_annotated_frames,
                inference_semaphore,
                config: Arc::new(config),
                metrics_hub,
                latest_results: Arc::new(DashMap::new()),
                tracker_runtimes: Arc::new(DashMap::new()),
                started_at: Instant::now(),
            })
        }

        /// Get a reference to the model registry.
        pub fn model_registry(&self) -> &Arc<ModelRegistry> {
            &self.model_registry
        }

        /// Get a reference to the centralised metrics hub.
        pub fn metrics_hub(&self) -> &Arc<NGMetricsHub> {
            &self.metrics_hub
        }
    }

    fn validate_model_id(model_id: &str) -> Result<(), AiEngineError> {
        if model_id.is_empty() {
            return Err(AiEngineError::PipelineConfigError(
                "model id cannot be empty".to_string(),
            ));
        }
        if model_id
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        {
            return Err(AiEngineError::PipelineConfigError(
                "model id contains invalid characters (allowed: a-z A-Z 0-9 _ -)".to_string(),
            ));
        }
        Ok(())
    }

    fn model_file_path(models_dir: &Path, model_id: &str, extension: &str) -> PathBuf {
        models_dir.join(format!("{model_id}.{extension}"))
    }

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

    fn remap_context_to_full_frame(
        context: &mut PipelineContext,
        roi: &RegionOfInterest,
        full_width: u32,
        full_height: u32,
    ) {
        for det in &mut context.detections {
            det.bbox = remap_bbox_to_full_frame(det.bbox, roi);
        }

        for kpd in &mut context.keypoint_detections {
            kpd.bbox = remap_bbox_to_full_frame(kpd.bbox, roi);
            for kp in &mut kpd.keypoints {
                kp.x = remap_scalar_to_full_frame(kp.x, roi.x_min, roi.x_max);
                kp.y = remap_scalar_to_full_frame(kp.y, roi.y_min, roi.y_max);
            }
        }

        for mask in &mut context.segmentation_masks {
            remap_segmentation_mask_to_full_frame_in_place(mask, roi, full_width, full_height);
        }
    }

    #[inline]
    fn remap_bbox_to_full_frame(bbox: BoundingBox, roi: &RegionOfInterest) -> BoundingBox {
        BoundingBox {
            x_min: remap_scalar_to_full_frame(bbox.x_min, roi.x_min, roi.x_max),
            y_min: remap_scalar_to_full_frame(bbox.y_min, roi.y_min, roi.y_max),
            x_max: remap_scalar_to_full_frame(bbox.x_max, roi.x_min, roi.x_max),
            y_max: remap_scalar_to_full_frame(bbox.y_max, roi.y_min, roi.y_max),
        }
    }

    #[inline]
    fn remap_scalar_to_full_frame(v: f32, min: f32, max: f32) -> f32 {
        (min + v * (max - min)).clamp(0.0, 1.0)
    }

    fn remap_segmentation_mask_to_full_frame_in_place(
        mask: &mut SegmentationMask,
        roi: &RegionOfInterest,
        full_width: u32,
        full_height: u32,
    ) {
        let fw = full_width as usize;
        let fh = full_height as usize;
        if fw == 0 || fh == 0 || mask.width == 0 || mask.height == 0 {
            return;
        }

        let mut full_mask = vec![0u8; fw * fh];
        let x_start = (roi.x_min * full_width as f32)
            .floor()
            .clamp(0.0, full_width as f32) as u32;
        let y_start = (roi.y_min * full_height as f32)
            .floor()
            .clamp(0.0, full_height as f32) as u32;
        let x_end = (roi.x_max * full_width as f32)
            .ceil()
            .clamp(0.0, full_width as f32) as u32;
        let y_end = (roi.y_max * full_height as f32)
            .ceil()
            .clamp(0.0, full_height as f32) as u32;

        let roi_w = (x_end.saturating_sub(x_start)).max(1);
        let roi_h = (y_end.saturating_sub(y_start)).max(1);
        let mw = mask.width as usize;
        let mh = mask.height as usize;
        if mask.mask.len() != mw * mh {
            return;
        }

        for gy in y_start..y_end {
            for gx in x_start..x_end {
                let local_x = gx.saturating_sub(x_start) as f32 / roi_w as f32;
                let local_y = gy.saturating_sub(y_start) as f32 / roi_h as f32;
                let mx =
                    ((local_x * mask.width as f32).floor() as u32).min(mask.width - 1) as usize;
                let my =
                    ((local_y * mask.height as f32).floor() as u32).min(mask.height - 1) as usize;
                let class_idx = mask.mask[my * mw + mx];
                full_mask[gy as usize * fw + gx as usize] = class_idx;
            }
        }

        mask.mask = full_mask;
        mask.width = full_width;
        mask.height = full_height;
    }

    #[inline]
    fn usize_to_u64_saturating(value: usize) -> u64 {
        u64::try_from(value).unwrap_or(u64::MAX)
    }

    #[inline]
    fn duration_nanos_u64_saturating(duration: std::time::Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }

    #[inline]
    fn estimate_vec_payload_bytes<T>(values: &[T]) -> u64 {
        let elem_size = std::mem::size_of::<T>();
        let bytes = values.len().saturating_mul(elem_size);
        usize_to_u64_saturating(bytes)
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
            .map(|jpeg| usize_to_u64_saturating(jpeg.len()))
            .unwrap_or(0)
    }

    /// Background annotation worker loop.
    ///
    /// Rendering runs off the inference hot path. When queue pressure is high,
    /// requests can be dropped by caller-side `try_send`.
    async fn annotation_worker_loop(
        mut rx: mpsc::Receiver<AnnotateRequest>,
        annotator: Arc<dyn FrameAnnotator>,
        latest_annotated_frames: Arc<DashMap<i32, Bytes>>,
    ) {
        while let Some(request) = rx.recv().await {
            let channel_id = request.channel_id;
            let frame_seq = request.core.frame_seq;
            let annotate_exec_span = info_span!(
                "annotate_exec",
                channel_id = channel_id,
                frame_seq = frame_seq
            );
            let _annotate_exec_guard = annotate_exec_span.enter();

            let frame = Arc::clone(&request.frame);
            let core = Arc::clone(&request.core);
            let config = Arc::clone(&request.config);
            let worker_annotator = Arc::clone(&annotator);

            let annotated = tokio::task::spawn_blocking(move || {
                worker_annotator.annotate(frame.as_ref(), core.as_ref(), config.as_ref())
            })
            .await;

            match annotated {
                Ok(Ok(jpeg)) => {
                    latest_annotated_frames.insert(channel_id, jpeg);
                }
                Ok(Err(error)) => {
                    warn!(
                        channel_id,
                        frame_seq,
                        error = %error,
                        "background annotation failed"
                    );
                }
                Err(join_error) => {
                    warn!(
                        channel_id,
                        frame_seq,
                        error = %join_error,
                        "background annotation worker panicked"
                    );
                }
            }
        }
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

    #[async_trait::async_trait]
    impl AiEngineApi for AiEngine {
        async fn analyze_frame(
            &self,
            request: FrameAnalysisRequest,
        ) -> Result<AnalysisResult, AiEngineError> {
            let start = std::time::Instant::now();
            let channel_id = request.channel_id;
            let frame_seq = request.frame.seq;

            let ai = self.metrics_hub.ai();
            ai.inc_frames_submitted(&channel_id.to_string());

            // 1. Acquire inference permit (backpressure)
            let _permit = self.inference_semaphore.try_acquire().map_err(|_| {
                debug!(
                    channel_id,
                    available = self.inference_semaphore.available_permits(),
                    "frame dropped — inference queue at capacity"
                );
                ai.inc_frames_dropped(&channel_id.to_string());
                AiEngineError::Backpressure
            })?;

            ai.inc_active_inferences();
            let _guard = scopeguard::guard((), |_| {
                self.metrics_hub.ai().dec_active_inferences();
            });

            // 2. Resolve pipeline
            let pipeline = self
                .pipelines
                .get(&channel_id)
                .map(|p| Arc::clone(p.value()))
                .ok_or_else(|| {
                    warn!(
                        channel_id,
                        "frame rejected — no pipeline registered for channel"
                    );
                    AiEngineError::PipelineNotFound(channel_id)
                })?;

            // 3. Decode frame (if encoded)
            let decode_wait_span = info_span!("decode_wait", channel_id = channel_id);
            let _decode_wait_guard = decode_wait_span.enter();
            drop(_decode_wait_guard);
            let decode_exec_span =
                info_span!("decode_exec", channel_id = channel_id, format = ?request.frame.format);
            let _decode_exec_guard = decode_exec_span.enter();
            let decoded = self
                .frame_decoder
                .decode(&request.frame)
                .await
                .map_err(|e| {
                    warn!(
                        channel_id,
                        format = ?request.frame.format,
                        frame_size = request.frame.data.len(),
                        error = %e,
                        "frame decode failed"
                    );
                    e
                })?;
            drop(_decode_exec_guard);

            // 4. Resolve and process ROI regions.
            let rois = resolve_effective_rois(
                request.roi,
                pipeline.compiled.roi,
                pipeline.compiled.roi_regions.as_ref(),
            )?;
            let mut merged = PipelineContext::new(decoded.clone());

            for roi in rois.iter() {
                let roi_frame = if is_full_frame_roi(roi) {
                    decoded.clone()
                } else {
                    crop_frame(&decoded, roi)?
                };

                // 5. Execute pipeline stages on one ROI.
                let mut context = PipelineContext::new(roi_frame);
                for stage in pipeline.compiled.stages.iter() {
                    match stage {
                        CompiledStage::Inference(stage) => {
                            let compiled_model =
                                pipeline.compiled.model(stage.model).ok_or_else(|| {
                                    AiEngineError::InternalError(format!(
                                        "compiled model handle {} not found",
                                        stage.model.0
                                    ))
                                })?;
                            let (raw_output, coord_transform, mut infer_timing) = self
                                .inference_pool
                                .infer_compiled(
                                    compiled_model.info.id.as_str(),
                                    &context.current_frame,
                                    stage.preprocessor.as_ref(),
                                    stage.input_shape.as_ref(),
                                    stage.input_dtype,
                                )
                                .await?;

                            let postprocess_span = info_span!(
                                "postprocess_exec",
                                model_id = compiled_model.info.id.as_str()
                            );
                            let postprocess_start = Instant::now();
                            let _postprocess_guard = postprocess_span.enter();
                            let post_result = stage.postprocessor.process(
                                &raw_output,
                                &coord_transform,
                                &compiled_model.info.labels,
                            )?;
                            infer_timing.postprocess = postprocess_start.elapsed();
                            drop(_postprocess_guard);
                            ai.add_lock_wait_ns(duration_nanos_u64_saturating(
                                infer_timing.infer_wait,
                            ));

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
                            let mut runtime =
                                self.tracker_runtimes.entry(channel_id).or_insert_with(|| {
                                    TrackerRuntime::new(algorithm.clone(), *max_age)
                                });

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
                                .wasm_runtime
                                .execute_frame_transform(
                                    module_id.as_ref(),
                                    &context.current_frame,
                                    Arc::clone(config),
                                )
                                .await
                                .map_err(|e| {
                                    warn!(
                                        channel_id,
                                        module_id = module_id.as_ref(),
                                        error = %e,
                                        "WASM frame transform failed"
                                    );
                                    e
                                })?;
                            context.current_frame = transformed;

                            ai.observe_wasm_execution_latency(
                                module_id.as_ref(),
                                start.elapsed().as_secs_f64(),
                            );
                        }

                        CompiledStage::ResultProcessor {
                            stage: _,
                            module_id,
                            config,
                        } => {
                            let output = self
                                .wasm_runtime
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
                                    warn!(
                                        channel_id,
                                        module_id = module_id.as_ref(),
                                        error = %e,
                                        "WASM result processor failed"
                                    );
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

                            ai.observe_wasm_execution_latency(
                                module_id.as_ref(),
                                start.elapsed().as_secs_f64(),
                            );
                        }
                    }
                }

                if !is_full_frame_roi(roi) {
                    remap_context_to_full_frame(&mut context, roi, decoded.width, decoded.height);
                }

                merged.add_detections(context.detections);
                merged.add_classifications(context.classifications);
                merged.add_keypoint_detections(context.keypoint_detections);
                merged.add_segmentation_masks(context.segmentation_masks);
                merged.add_anomaly_maps(context.anomaly_maps);
                merged.custom_outputs.extend(context.custom_outputs);
            }

            // 6. Evaluate alarm rules
            let alarms = evaluate_alarm_rules(pipeline.compiled.alarm_rules.as_ref(), &merged);

            // 7. Record metrics
            let latency = start.elapsed();
            for stage in pipeline.compiled.stages.iter() {
                if let CompiledStage::Inference(compiled_stage) = stage {
                    if let Some(model) = pipeline.compiled.model(compiled_stage.model) {
                        ai.record_inference(latency, model.info.id.as_str());
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

            // 8. Enqueue async annotation request (non-blocking hot path)
            let estimated_core_bytes = estimate_core_payload_bytes(core.as_ref());
            ai.add_alloc_estimate(estimated_core_bytes);
            let annotate_wait_span = info_span!(
                "annotate_wait",
                channel_id = channel_id,
                frame_seq = frame_seq
            );
            let _annotate_wait_guard = annotate_wait_span.enter();
            let annotation_config = Arc::new(pipeline.compiled.annotation.clone());
            let frame_for_annotation = Arc::new(decoded.clone());
            let build_request = || AnnotateRequest {
                channel_id,
                frame: Arc::clone(&frame_for_annotation),
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
                                frame_seq, "annotation queue full, dropping newest request"
                            );
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            warn!(
                                channel_id,
                                frame_seq, "annotation queue closed, dropping annotation request"
                            );
                        }
                    }
                }
                AnnotationQueueOverflowStrategy::WaitForSlot => {
                    let timeout =
                        std::time::Duration::from_millis(annotation_config.enqueue_timeout_ms);
                    match tokio::time::timeout(timeout, self.annotate_tx.send(build_request()))
                        .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => {
                            warn!(
                                channel_id,
                                frame_seq, "annotation queue closed, dropping annotation request"
                            );
                        }
                        Err(_) => {
                            warn!(
                                channel_id,
                                frame_seq,
                                timeout_ms = annotation_config.enqueue_timeout_ms,
                                "annotation enqueue timeout, dropping request"
                            );
                        }
                    }
                }
            }
            drop(_annotate_wait_guard);
            let render = RenderArtifact {
                annotated_frame: self
                    .latest_annotated_frames
                    .get(&channel_id)
                    .map(|jpeg| jpeg.value().clone()),
            };

            // 9. Build result
            let frame_timestamp = request.frame.timestamp;
            let result = build_analysis_result(core.as_ref(), frame_timestamp, latency, &render);

            // Cache latest result for snapshot API
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
                frame_seq = frame_seq,
                detections = result.detections.len(),
                classifications = result.classifications.len(),
                alarms = result.alarms.len(),
                latency_ms = latency.as_millis() as u64,
                annotated = result.annotated_frame.is_some(),
                "frame analysis complete"
            );

            Ok(result)
        }

        fn has_capacity(&self, _pipeline_id: &PipelineId) -> bool {
            self.inference_semaphore.available_permits() > 0
        }

        async fn list_models(&self) -> Result<Vec<Arc<ModelInfo>>, AiEngineError> {
            self.model_registry.list_all().await
        }

        async fn get_pipeline(
            &self,
            channel_id: i32,
        ) -> Result<Option<Arc<PipelineConfig>>, AiEngineError> {
            Ok(self
                .pipelines
                .get(&channel_id)
                .map(|p| Arc::clone(&p.value().config)))
        }

        fn register_pipeline(
            &self,
            channel_id: i32,
            config: PipelineConfig,
        ) -> Result<(), AiEngineError> {
            let report = config.validate_dag();
            if !report.valid {
                let joined = report.errors.join("; ");
                warn!(
                    channel_id,
                    pipeline_name = %config.name,
                    errors = %joined,
                    "pipeline registration rejected due to invalid DAG"
                );
                return Err(AiEngineError::PipelineConfigError(joined));
            }

            let compiled =
                compile_pipeline(&config, self.model_registry.as_ref()).map_err(|e| {
                    warn!(
                        channel_id,
                        pipeline_name = %config.name,
                        error = %e,
                        "pipeline registration rejected during compilation"
                    );
                    e
                })?;
            let stages = config.stages.len();
            info!(
                channel_id,
                pipeline_name = %config.name,
                stages,
                "pipeline registered"
            );
            self.pipelines.insert(
                channel_id,
                Arc::new(RegisteredPipeline {
                    config: Arc::new(config),
                    compiled: Arc::new(compiled),
                }),
            );
            self.tracker_runtimes.remove(&channel_id);
            Ok(())
        }

        fn unregister_pipeline(&self, channel_id: i32) {
            if self.pipelines.remove(&channel_id).is_some() {
                self.tracker_runtimes.remove(&channel_id);
                self.latest_annotated_frames.remove(&channel_id);
                info!(channel_id, "pipeline unregistered");
            }
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

        async fn get_model(&self, model_id: &str) -> Result<Option<Arc<ModelInfo>>, AiEngineError> {
            Ok(self.model_registry.get(model_id).await)
        }

        async fn upload_model(
            &self,
            onnx_bytes: Bytes,
            metadata: ModelUploadMetadata,
        ) -> Result<Arc<ModelInfo>, AiEngineError> {
            validate_model_id(&metadata.id)?;

            let models_dir = self.model_registry.models_dir().to_path_buf();
            let onnx_path = model_file_path(&models_dir, &metadata.id, "onnx");
            let labels_path = model_file_path(&models_dir, &metadata.id, "labels.txt");
            let sidecar_path = model_file_path(&models_dir, &metadata.id, "json");

            if tokio::fs::try_exists(&onnx_path)
                .await
                .map_err(|e| AiEngineError::IoError(e.to_string()))?
            {
                return Err(AiEngineError::PipelineConfigError(format!(
                    "model '{}' already exists",
                    metadata.id
                )));
            }

            tokio::fs::write(&onnx_path, &onnx_bytes)
                .await
                .map_err(|e| AiEngineError::IoError(e.to_string()))?;

            if !metadata.labels.is_empty() {
                let labels_content = metadata.labels.join("\n");
                tokio::fs::write(&labels_path, labels_content)
                    .await
                    .map_err(|e| AiEngineError::IoError(e.to_string()))?;
            }

            let sidecar = serde_json::json!({
                "task": metadata.task,
                "inputs": [],
                "outputs": [],
            });
            tokio::fs::write(
                &sidecar_path,
                serde_json::to_vec_pretty(&sidecar)
                    .map_err(|e| AiEngineError::IoError(e.to_string()))?,
            )
            .await
            .map_err(|e| AiEngineError::IoError(e.to_string()))?;

            let file_size = tokio::fs::metadata(&onnx_path)
                .await
                .map_err(|e| AiEngineError::IoError(e.to_string()))?
                .len();

            let info = ModelInfo {
                id: metadata.id,
                name: metadata.name,
                version: metadata.version,
                format: ModelFormat::Onnx,
                path: onnx_path,
                inputs: vec![],
                outputs: vec![],
                task: metadata.task,
                labels: metadata.labels,
                default_preprocess: metadata.default_preprocess,
                default_postprocess: metadata.default_postprocess,
                loaded: false,
                file_size,
            };
            let info = Arc::new(info);
            self.model_registry.upsert_shared(Arc::clone(&info));
            Ok(info)
        }

        async fn update_model(
            &self,
            model_id: &str,
            request: ModelUpdateRequest,
        ) -> Result<Arc<ModelInfo>, AiEngineError> {
            self.model_registry
                .update_model_metadata(model_id, request)?;

            self.model_registry
                .get(model_id)
                .await
                .ok_or_else(|| AiEngineError::ModelNotFound(model_id.to_string()))
        }

        async fn delete_model(&self, model_id: &str) -> Result<(), AiEngineError> {
            self.inference_pool.unload(model_id);
            self.model_registry.remove(model_id);

            let models_dir = self.model_registry.models_dir().to_path_buf();
            let onnx_path = model_file_path(&models_dir, model_id, "onnx");
            let labels_path = model_file_path(&models_dir, model_id, "labels.txt");
            let sidecar_path = model_file_path(&models_dir, model_id, "json");

            let _ = tokio::fs::remove_file(&onnx_path).await;
            let _ = tokio::fs::remove_file(&labels_path).await;
            let _ = tokio::fs::remove_file(&sidecar_path).await;

            Ok(())
        }

        async fn load_model(&self, model_id: &str) -> Result<(), AiEngineError> {
            self.inference_pool.load(model_id).await
        }

        async fn unload_model(&self, model_id: &str) -> Result<(), AiEngineError> {
            self.inference_pool.unload(model_id);
            Ok(())
        }

        async fn list_pipelines(&self) -> Result<Vec<(i32, Arc<PipelineConfig>)>, AiEngineError> {
            Ok(self
                .pipelines
                .iter()
                .map(|entry| (*entry.key(), Arc::clone(&entry.value().config)))
                .collect())
        }

        async fn upsert_pipeline(
            &self,
            request: PipelineUpsertRequest,
        ) -> Result<(), AiEngineError> {
            self.register_pipeline(request.channel_id, request.config)
        }

        async fn delete_pipeline(&self, channel_id: i32) -> Result<(), AiEngineError> {
            self.unregister_pipeline(channel_id);
            Ok(())
        }

        async fn get_engine_status(&self) -> Result<EngineStatus, AiEngineError> {
            let models = self.model_registry.list_all().await?;
            let loaded_count = self.inference_pool.loaded_count();

            let total_memory_bytes: u64 = models
                .iter()
                .filter(|m| m.loaded)
                .map(|m| m.file_size)
                .sum();

            Ok(EngineStatus {
                enabled: true,
                execution_provider: self.config.inference.execution_provider.clone(),
                models: EngineModelStatus {
                    registered: models.len(),
                    loaded: loaded_count,
                    total_memory_bytes,
                },
                inference: EngineInferenceStatus {
                    active_count: self.metrics_hub.ai().active_inference_count(),
                    max_concurrent: self.config.max_concurrent_inferences,
                    available_permits: self.inference_semaphore.available_permits(),
                    total_inferences: self.metrics_hub.ai().total_inferences(),
                    avg_latency_ms: self.metrics_hub.ai().avg_latency_ms(),
                },
                pipelines: EnginePipelineStatus {
                    registered: self.pipelines.len(),
                    active_channels: self.pipelines.len(),
                },
                algorithms: EngineAlgorithmStatus {
                    registered: self.wasm_runtime.algorithm_count(),
                    wasm_modules: self.wasm_runtime.algorithm_count(),
                },
                decoder: EngineDecoderStatus {
                    workers: self.config.decoder_workers,
                    queue_depth: 0,
                },
                uptime_secs: self.started_at.elapsed().as_secs(),
            })
        }

        fn list_preprocessors(&self) -> Vec<ProcessorInfo> {
            builtin_preprocessors()
        }

        fn list_postprocessors(&self) -> Vec<ProcessorInfo> {
            builtin_postprocessors()
        }

        // ── Algorithm management ──────────────────────────────────

        async fn list_algorithms(&self) -> Result<Vec<Arc<WasmAlgorithmInfo>>, AiEngineError> {
            Ok(self.wasm_runtime.list_algorithms())
        }

        async fn get_algorithm(
            &self,
            algorithm_id: &str,
        ) -> Result<Option<Arc<WasmAlgorithmInfo>>, AiEngineError> {
            Ok(self.wasm_runtime.get_algorithm(algorithm_id))
        }

        async fn upload_algorithm(
            &self,
            wasm_bytes: Bytes,
            metadata: AlgorithmUploadMetadata,
        ) -> Result<Arc<WasmAlgorithmInfo>, AiEngineError> {
            self.wasm_runtime
                .upload_algorithm(wasm_bytes, metadata)
                .await
        }

        async fn delete_algorithm(&self, algorithm_id: &str) -> Result<(), AiEngineError> {
            self.wasm_runtime.delete_algorithm(algorithm_id).await
        }

        async fn test_algorithm(
            &self,
            algorithm_id: &str,
            test_input: AlgorithmTestInput,
        ) -> Result<AlgorithmTestResult, AiEngineError> {
            self.wasm_runtime
                .test_algorithm(algorithm_id, test_input)
                .await
        }
    }

    /// Static list of built-in preprocessors.
    fn builtin_preprocessors() -> Vec<ProcessorInfo> {
        vec![
            ProcessorInfo {
                id: "letterbox".into(),
                name: "Letterbox Resize".into(),
                description: "Preserves aspect ratio with padding. Standard for YOLO models."
                    .into(),
                applicable_tasks: vec!["object_detection".into(), "segmentation".into()],
                parameters: vec![
                    ProcessorParameter {
                        name: "pad_value".into(),
                        description: "Fill value for padding pixels (0-255).".into(),
                        param_type: ParamType::U8,
                        default: Some(serde_json::json!(114)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "normalization".into(),
                        description: "Normalization preset: yolo, imagenet, symmetric, or custom."
                            .into(),
                        param_type: ParamType::String,
                        default: Some(serde_json::json!("yolo")),
                        required: false,
                    },
                ],
            },
            ProcessorInfo {
                id: "center_crop".into(),
                name: "Center Crop".into(),
                description:
                    "Crops the center region and resizes. Common for classification models.".into(),
                applicable_tasks: vec!["classification".into()],
                parameters: vec![ProcessorParameter {
                    name: "normalization".into(),
                    description: "Normalization preset: yolo, imagenet, symmetric, or custom."
                        .into(),
                    param_type: ParamType::String,
                    default: Some(serde_json::json!("imagenet")),
                    required: false,
                }],
            },
            ProcessorInfo {
                id: "direct_resize".into(),
                name: "Direct Resize".into(),
                description: "Directly resizes to target dimensions (may distort aspect ratio)."
                    .into(),
                applicable_tasks: vec![
                    "object_detection".into(),
                    "classification".into(),
                    "segmentation".into(),
                ],
                parameters: vec![ProcessorParameter {
                    name: "normalization".into(),
                    description: "Normalization preset: yolo, imagenet, symmetric, or custom."
                        .into(),
                    param_type: ParamType::String,
                    default: Some(serde_json::json!("yolo")),
                    required: false,
                }],
            },
        ]
    }

    /// Static list of built-in postprocessors.
    fn builtin_postprocessors() -> Vec<ProcessorInfo> {
        vec![
            ProcessorInfo {
                id: "yolov8_detection".into(),
                name: "YOLOv8 Detection".into(),
                description: "Post-processes YOLOv8 detection output (transposed format). \
                              Applies confidence thresholding and NMS."
                    .into(),
                applicable_tasks: vec!["object_detection".into()],
                parameters: vec![
                    ProcessorParameter {
                        name: "confidence_threshold".into(),
                        description: "Minimum confidence score to keep a detection.".into(),
                        param_type: ParamType::F32,
                        default: Some(serde_json::json!(0.5)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "nms_iou_threshold".into(),
                        description: "IoU threshold for Non-Maximum Suppression.".into(),
                        param_type: ParamType::F32,
                        default: Some(serde_json::json!(0.45)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "max_detections".into(),
                        description: "Maximum number of detections to keep after NMS.".into(),
                        param_type: ParamType::Usize,
                        default: Some(serde_json::json!(300)),
                        required: false,
                    },
                ],
            },
            ProcessorInfo {
                id: "yolov5_detection".into(),
                name: "YOLOv5 Detection".into(),
                description: "Post-processes YOLOv5 detection output (standard format). \
                              Applies confidence thresholding and NMS."
                    .into(),
                applicable_tasks: vec!["object_detection".into()],
                parameters: vec![
                    ProcessorParameter {
                        name: "confidence_threshold".into(),
                        description: "Minimum confidence score to keep a detection.".into(),
                        param_type: ParamType::F32,
                        default: Some(serde_json::json!(0.5)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "nms_iou_threshold".into(),
                        description: "IoU threshold for Non-Maximum Suppression.".into(),
                        param_type: ParamType::F32,
                        default: Some(serde_json::json!(0.45)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "max_detections".into(),
                        description: "Maximum number of detections to keep after NMS.".into(),
                        param_type: ParamType::Usize,
                        default: Some(serde_json::json!(300)),
                        required: false,
                    },
                ],
            },
            ProcessorInfo {
                id: "classification".into(),
                name: "Classification (Softmax + Top-K)".into(),
                description: "Applies softmax and returns top-K class predictions.".into(),
                applicable_tasks: vec!["classification".into()],
                parameters: vec![
                    ProcessorParameter {
                        name: "top_k".into(),
                        description: "Number of top predictions to return.".into(),
                        param_type: ParamType::Usize,
                        default: Some(serde_json::json!(5)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "apply_softmax".into(),
                        description: "Whether to apply softmax to raw logits.".into(),
                        param_type: ParamType::Bool,
                        default: Some(serde_json::json!(true)),
                        required: false,
                    },
                ],
            },
            ProcessorInfo {
                id: "segmentation".into(),
                name: "Semantic Segmentation".into(),
                description: "Performs argmax on [1,C,H,W] tensor to produce \
                              per-pixel class mask. For semantic segmentation models."
                    .into(),
                applicable_tasks: vec!["segmentation".into()],
                parameters: vec![],
            },
            ProcessorInfo {
                id: "yolov8_pose".into(),
                name: "YOLOv8 Pose / Keypoint Detection".into(),
                description: "Post-processes YOLOv8-Pose output with bbox + keypoints. \
                              Applies confidence thresholding and NMS."
                    .into(),
                applicable_tasks: vec!["object_detection".into()],
                parameters: vec![
                    ProcessorParameter {
                        name: "confidence_threshold".into(),
                        description: "Minimum confidence score to keep a detection.".into(),
                        param_type: ParamType::F32,
                        default: Some(serde_json::json!(0.5)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "nms_iou_threshold".into(),
                        description: "IoU threshold for Non-Maximum Suppression.".into(),
                        param_type: ParamType::F32,
                        default: Some(serde_json::json!(0.45)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "max_detections".into(),
                        description: "Maximum detections to keep after NMS.".into(),
                        param_type: ParamType::Usize,
                        default: Some(serde_json::json!(100)),
                        required: false,
                    },
                    ProcessorParameter {
                        name: "num_keypoints".into(),
                        description: "Number of keypoints per detection (17 for COCO).".into(),
                        param_type: ParamType::Usize,
                        default: Some(serde_json::json!(17)),
                        required: false,
                    },
                ],
            },
            ProcessorInfo {
                id: "anomaly_detection".into(),
                name: "Anomaly Detection".into(),
                description: "Extracts anomaly score and optional spatial heatmap. \
                              Compares score against threshold to determine anomaly flag."
                    .into(),
                applicable_tasks: vec!["anomaly_detection".into()],
                parameters: vec![ProcessorParameter {
                    name: "anomaly_threshold".into(),
                    description: "Score threshold for anomaly determination.".into(),
                    param_type: ParamType::F32,
                    default: Some(serde_json::json!(0.5)),
                    required: false,
                }],
            },
            ProcessorInfo {
                id: "passthrough".into(),
                name: "Passthrough".into(),
                description: "Returns raw model outputs without processing. \
                              Useful for custom models or debugging."
                    .into(),
                applicable_tasks: vec![
                    "object_detection".into(),
                    "classification".into(),
                    "segmentation".into(),
                    "anomaly_detection".into(),
                ],
                parameters: vec![],
            },
        ]
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
