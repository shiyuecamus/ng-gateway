//! AI Processing Engine — gateway-global singleton.
//!
//! Manages model lifecycle, inference worker pools, and pipeline orchestration.
//! All camera drivers share this single engine instance.

#[cfg(feature = "engine")]
mod inner {
    use crate::{
        algorithm::host::WasmAlgorithmHost,
        frame::decode::FrameDecoderPool,
        inference::pool::InferencePool,
        model::{profile::auto_detect_profile, registry::ModelRegistry},
        pipeline::{
            annotator::{DefaultFrameAnnotator, FrameAnnotator},
            context::PipelineContext,
            postprocess::PostprocessOutput,
            roi::crop_frame,
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
            model::ModelInfo,
            pipeline::{PipelineConfig, StageConfig},
            types::{
                AnalysisResult, Classification, Detection, EngineAlgorithmStatus,
                EngineDecoderStatus, EngineInferenceStatus, EngineModelStatus,
                EnginePipelineStatus, EngineStatus, FrameAnalysisRequest, ParamType, PipelineId,
                ProcessorInfo, ProcessorParameter,
            },
        },
        settings::AiEngineConfig,
    };
    use std::{sync::Arc, time::Instant};
    use tokio::sync::Semaphore;
    use tracing::{debug, info, warn};

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
        pipelines: Arc<DashMap<i32, Arc<PipelineConfig>>>,
        /// Frame decoder pool (JPEG / H.264 / H.265 → RGB24).
        frame_decoder: Arc<FrameDecoderPool>,
        /// WASM algorithm runtime (FrameTransform + ResultProcessor execution).
        wasm_runtime: Arc<WasmAlgorithmHost>,
        /// Frame annotator (draws BBox, labels, confidence, tracking IDs).
        annotator: Arc<dyn FrameAnnotator>,
        /// Global inference semaphore (bounds total concurrent inferences).
        inference_semaphore: Arc<Semaphore>,
        /// Engine configuration (retained for runtime reconfiguration and status API).
        config: Arc<AiEngineConfig>,
        /// Centralised metrics hub (shared with all gateway subsystems).
        metrics_hub: Arc<NGMetricsHub>,
        /// Latest analysis result per channel (for snapshot API).
        latest_results: Arc<DashMap<i32, AnalysisResult>>,
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

            Ok(Self {
                model_registry,
                inference_pool,
                pipelines: Arc::new(DashMap::new()),
                frame_decoder,
                wasm_runtime,
                annotator: Arc::new(DefaultFrameAnnotator),
                inference_semaphore,
                config: Arc::new(config),
                metrics_hub,
                latest_results: Arc::new(DashMap::new()),
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

    #[async_trait::async_trait]
    impl AiEngineApi for AiEngine {
        async fn analyze_frame(
            &self,
            request: FrameAnalysisRequest,
        ) -> Result<AnalysisResult, AiEngineError> {
            let start = std::time::Instant::now();
            let channel_id = request.channel_id;

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

            // 4. Apply ROI crop
            let cropped = if let Some(roi) = request.roi.or(pipeline.roi) {
                crop_frame(&decoded, &roi)?
            } else {
                decoded
            };

            // 5. Execute pipeline stages
            let mut context = PipelineContext::new(cropped);

            for stage in &pipeline.stages {
                match stage {
                    StageConfig::Inference {
                        model_id,
                        confidence_threshold: _,
                        nms_iou_threshold: _,
                        input_size,
                        preprocess: _pre_cfg,
                        postprocess: _post_cfg,
                    } => {
                        // Resolve model profile (pre+post processors)
                        let model_info =
                            self.model_registry.get(model_id).await.ok_or_else(|| {
                                warn!(channel_id, model_id, "model not found in registry");
                                AiEngineError::ModelNotFound(model_id.clone())
                            })?;

                        let profile = auto_detect_profile(&model_info);

                        // Run inference
                        let (raw_output, coord_transform) = self
                            .inference_pool
                            .infer(
                                model_id,
                                &context.current_frame,
                                profile.preprocessor.as_ref(),
                                *input_size,
                            )
                            .await?;

                        // Postprocess
                        let post_result = profile.postprocessor.process(
                            &raw_output,
                            &coord_transform,
                            &model_info.labels,
                        )?;

                        context.add_detections(post_result.detections);
                        context.add_classifications(post_result.classifications);
                        context.add_keypoint_detections(post_result.keypoint_detections);
                        context.add_segmentation_masks(post_result.segmentation_masks);
                        context.add_anomaly_maps(post_result.anomaly_maps);
                        context.custom_outputs.extend(post_result.custom_outputs);
                    }

                    StageConfig::Tracker { .. } => {
                        // Phase 2.2: SORT/DeepSORT tracking (implemented in Phase 2.2)
                        debug!("tracker stage skipped (not yet implemented)");
                    }

                    StageConfig::FrameTransform { module_id, config } => {
                        let transformed = self
                            .wasm_runtime
                            .execute_frame_transform(module_id, &context.current_frame, config)
                            .await
                            .map_err(|e| {
                                warn!(
                                    channel_id,
                                    module_id,
                                    error = %e,
                                    "WASM frame transform failed"
                                );
                                e
                            })?;
                        context.current_frame = transformed;

                        ai.observe_wasm_execution_latency(module_id, start.elapsed().as_secs_f64());
                    }

                    StageConfig::ResultProcessor { module_id, config } => {
                        let output = self
                            .wasm_runtime
                            .execute_result_processor(
                                module_id,
                                &context.detections,
                                &context.classifications,
                                context.current_frame.width,
                                context.current_frame.height,
                                config,
                            )
                            .await
                            .map_err(|e| {
                                warn!(
                                    channel_id,
                                    module_id,
                                    error = %e,
                                    "WASM result processor failed"
                                );
                                e
                            })?;

                        // Replace detections and classifications with WASM output
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

                        ai.observe_wasm_execution_latency(module_id, start.elapsed().as_secs_f64());
                    }
                }
            }

            // 6. Evaluate alarm rules
            let alarms = evaluate_alarm_rules(&pipeline.alarm_rules, &context);

            // 7. Record metrics
            let latency = start.elapsed();
            for stage in &pipeline.stages {
                if let StageConfig::Inference { model_id, .. } = stage {
                    ai.record_inference(latency, model_id);
                }
            }
            for det in &context.detections {
                ai.inc_detections(&det.class);
            }
            for alarm in &alarms {
                let severity_str = format!("{:?}", alarm.severity);
                ai.inc_alarms_triggered(alarm.alarm_type.as_ref(), &severity_str);
            }

            // 8. Generate annotated frame (JPEG with overlaid results)
            let post_output = PostprocessOutput {
                detections: context.detections.clone(),
                classifications: context.classifications.clone(),
                keypoint_detections: context.keypoint_detections.clone(),
                segmentation_masks: context.segmentation_masks.clone(),
                anomaly_maps: context.anomaly_maps.clone(),
                custom_outputs: context.custom_outputs.clone(),
            };
            let annotated_frame = match self.annotator.annotate(
                &context.current_frame,
                &post_output,
                &pipeline.annotation,
            ) {
                Ok(jpeg) => Some(jpeg),
                Err(e) => {
                    tracing::warn!(channel_id, error = %e, "frame annotation failed, skipping");
                    None
                }
            };

            // 9. Build result
            let result = AnalysisResult {
                frame_seq: request.frame.seq,
                frame_timestamp: request.frame.timestamp,
                detections: context.detections,
                classifications: context.classifications,
                keypoint_detections: context.keypoint_detections,
                segmentation_masks: context.segmentation_masks,
                anomaly_maps: context.anomaly_maps,
                alarms,
                inference_latency: latency,
                annotated_frame,
            };

            // Cache latest result for snapshot API
            self.latest_results.insert(channel_id, result.clone());

            debug!(
                channel_id,
                frame_seq = request.frame.seq,
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

        async fn list_models(&self) -> Result<Vec<ModelInfo>, AiEngineError> {
            self.model_registry.list_all().await
        }

        async fn get_pipeline(
            &self,
            channel_id: i32,
        ) -> Result<Option<PipelineConfig>, AiEngineError> {
            Ok(self
                .pipelines
                .get(&channel_id)
                .map(|p| p.value().as_ref().clone()))
        }

        fn register_pipeline(&self, channel_id: i32, config: PipelineConfig) {
            let stages = config.stages.len();
            let name = config.name.clone();
            self.pipelines.insert(channel_id, Arc::new(config));
            info!(
                channel_id,
                pipeline_name = %name,
                stages,
                "pipeline registered"
            );
        }

        fn unregister_pipeline(&self, channel_id: i32) {
            if self.pipelines.remove(&channel_id).is_some() {
                info!(channel_id, "pipeline unregistered");
            }
        }

        async fn get_latest_result(
            &self,
            channel_id: i32,
        ) -> Result<Option<AnalysisResult>, AiEngineError> {
            Ok(self
                .latest_results
                .get(&channel_id)
                .map(|r| r.value().clone()))
        }

        async fn get_model(&self, model_id: &str) -> Result<Option<ModelInfo>, AiEngineError> {
            Ok(self.model_registry.get(model_id).await)
        }

        async fn list_pipelines(&self) -> Result<Vec<(i32, PipelineConfig)>, AiEngineError> {
            Ok(self
                .pipelines
                .iter()
                .map(|entry| (*entry.key(), entry.value().as_ref().clone()))
                .collect())
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

        async fn list_algorithms(&self) -> Result<Vec<WasmAlgorithmInfo>, AiEngineError> {
            Ok(self.wasm_runtime.list_algorithms())
        }

        async fn get_algorithm(
            &self,
            algorithm_id: &str,
        ) -> Result<Option<WasmAlgorithmInfo>, AiEngineError> {
            Ok(self.wasm_runtime.get_algorithm(algorithm_id))
        }

        async fn upload_algorithm(
            &self,
            wasm_bytes: Bytes,
            metadata: AlgorithmUploadMetadata,
        ) -> Result<WasmAlgorithmInfo, AiEngineError> {
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
