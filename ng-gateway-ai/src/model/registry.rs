//! Model registry — write-through cache with full lifecycle management.
//!
//! The registry is a write-through cache: all mutations go to the database
//! first, then the in-memory cache is updated. On startup the cache is
//! hydrated from DB records and validated against disk artifacts.
//!
//! This module implements [`AiModelRegistry`] and owns:
//! - Probe (via [`ProberRegistry`])
//! - Install (probe → DB insert → atomic file move → cache)
//! - Load / Unload (delegated to [`ModelBackend`])
//! - Update metadata (DB update → cache refresh)
//! - Uninstall (backend unload → file delete → DB delete → cache evict)

#[cfg(feature = "engine")]
mod inner {
    use crate::{
        inference::{
            backend::{InferTiming, ModelBackend},
            batch::BatchRouter,
            RawInferenceOutput,
        },
        model::prober::ProberRegistry,
        pipeline::preprocess::PreprocessOutput,
    };
    use dashmap::DashMap;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::{
        domain::prelude::{
            ModelInfo, ModelInstallRequest, ModelPageParams, ModelProbeInfo, NewModel, PageResult,
            UpdateModel,
        },
        enums::ai::ModelFormat,
        settings::BatchingConfig,
        AiModelRegistry,
    };
    use ng_gateway_repository::ModelRepository;
    use sea_orm::{DatabaseConnection, IntoActiveModel};
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };
    use tracing::{info, warn};

    /// Model registry — DB-backed write-through cache with full lifecycle.
    ///
    /// Keyed by `model_id` (i32). All mutations are DB-first, then cache.
    /// Maintains a secondary `key_index` for O(1) lookup by `key`,
    /// which is critical for the per-frame inference hot path.
    pub struct ModelRegistry {
        /// Cached model info keyed by model id.
        cache: DashMap<i32, Arc<ModelInfo>>,
        /// Reverse index: key -> model_id for O(1) key-based lookup.
        key_index: DashMap<String, i32>,
        /// Root directory for model artifact files.
        models_dir: PathBuf,
        /// Prober registry for extracting metadata from model files.
        prober_registry: Arc<ProberRegistry>,
        /// Inference backends keyed by model format.
        backends: Vec<Arc<dyn ModelBackend>>,
        /// Optional dynamic batching router.
        /// When present and enabled, `infer_by_key()` routes CPU tensor
        /// requests through the batch collector for aggregation.
        batch_router: Option<Arc<BatchRouter>>,
    }

    impl ModelRegistry {
        /// Initialize from database records (DB is the source of truth).
        ///
        /// Loads all model records and validates that artifact files exist.
        /// Models whose files are missing are logged as warnings and skipped.
        ///
        /// `db_conn` provides an externally-owned database connection for use
        /// during gateway startup before `NGAppContext` is initialized. When
        /// `None`, the repository falls back to `NGAppContext` (which must
        /// already be set).
        pub async fn new(
            models_dir: PathBuf,
            prober_registry: Arc<ProberRegistry>,
            backends: Vec<Arc<dyn ModelBackend>>,
            batching_config: Option<BatchingConfig>,
            db_conn: Option<&DatabaseConnection>,
        ) -> Result<Self, AiEngineError> {
            if !models_dir.exists() {
                info!(dir = %models_dir.display(), "models directory does not exist, creating");
                tokio::fs::create_dir_all(&models_dir)
                    .await
                    .map_err(|e| AiEngineError::IoError(e.to_string()))?;
            }

            let batch_router = batching_config.filter(|c| c.enabled).map(|c| {
                info!(
                    max_batch_size = c.max_batch_size,
                    collect_timeout_ms = c.collect_timeout_ms,
                    max_queue_depth = c.max_queue_depth,
                    adaptive = c.adaptive,
                    "dynamic batching enabled"
                );
                Arc::new(BatchRouter::new(c))
            });

            let registry = Self {
                cache: DashMap::new(),
                key_index: DashMap::new(),
                models_dir,
                prober_registry,
                backends,
                batch_router,
            };

            let db_models = ModelRepository::list_all(db_conn)
                .await
                .map_err(|e| AiEngineError::IoError(e.to_string()))?;

            for entity in db_models {
                let info = ModelInfo::from(entity);
                let artifact_path = Path::new(&info.path);
                if artifact_path.exists() {
                    registry.key_index.insert(info.key.clone(), info.id);
                    registry.cache.insert(info.id, Arc::new(info));
                } else {
                    warn!(
                        model_id = info.id,
                        key = %info.key,
                        path = %info.path,
                        "model artifact missing while restoring from DB"
                    );
                }
            }

            info!(
                count = registry.cache.len(),
                "model registry initialized from DB"
            );
            Ok(registry)
        }

        /// Look up a model by id from cache.
        #[inline]
        pub fn get(&self, model_id: i32) -> Option<Arc<ModelInfo>> {
            self.cache.get(&model_id).map(|r| Arc::clone(r.value()))
        }

        /// Look up a model by key from cache using O(1) reverse index.
        pub fn get_by_key(&self, key: &str) -> Option<Arc<ModelInfo>> {
            let model_id = *self.key_index.get(key)?;
            self.cache.get(&model_id).map(|r| Arc::clone(r.value()))
        }

        /// List all cached models.
        pub fn list_all(&self) -> Vec<Arc<ModelInfo>> {
            self.cache.iter().map(|r| Arc::clone(r.value())).collect()
        }

        /// Number of cached models.
        pub fn len(&self) -> usize {
            self.cache.len()
        }

        /// Check if the cache is empty.
        pub fn is_empty(&self) -> bool {
            self.cache.is_empty()
        }

        /// Get the models directory path.
        pub fn models_dir(&self) -> &Path {
            &self.models_dir
        }

        /// Find the appropriate backend for a model format (borrowed ref).
        fn backend_for(&self, format: ModelFormat) -> Option<&dyn ModelBackend> {
            self.backends
                .iter()
                .find(|b| b.format() == format)
                .map(|b| b.as_ref())
        }

        /// Find the appropriate backend Arc for a model format.
        fn backend_arc_for(&self, format: ModelFormat) -> Option<Arc<dyn ModelBackend>> {
            self.backends.iter().find(|b| b.format() == format).cloned()
        }

        /// Total loaded model count across all backends.
        pub fn total_loaded_count(&self) -> usize {
            self.backends.iter().map(|b| b.loaded_count()).sum()
        }

        /// Total estimated memory across all backends.
        pub fn total_estimated_memory_bytes(&self) -> u64 {
            self.backends
                .iter()
                .map(|b| b.estimated_memory_bytes())
                .sum()
        }

        /// Run inference on already-preprocessed input, routing to the
        /// correct backend based on model format.
        ///
        /// The caller (Engine layer) is responsible for running the
        /// appropriate preprocessor and producing the `PreprocessOutput`.
        /// This separation enables backend-specific input formats:
        /// ONNX receives `CpuTensor(f32 NCHW)`, RKNN receives
        /// `DeviceMemory(DMA-buf uint8 NHWC)`.
        pub async fn infer_by_key(
            &self,
            key: &str,
            input: PreprocessOutput,
        ) -> Result<(RawInferenceOutput, InferTiming), AiEngineError> {
            let info = self
                .get_by_key(key)
                .ok_or(AiEngineError::ModelNotFound(key.to_string()))?;

            let backend = self
                .backend_for(info.format)
                .ok_or(AiEngineError::ModelLoadError(format!(
                    "no backend for format {:?}",
                    info.format
                )))?;

            if !backend.is_loaded(info.id) {
                backend.load(info.id, Path::new(&info.path)).await?;
            }

            // Route through batch collector when enabled and the input
            // is a CPU tensor (GPU/DMA inputs bypass batching).
            if let Some(ref router) = self.batch_router {
                if router.is_enabled() && matches!(&input, PreprocessOutput::CpuTensor { .. }) {
                    let backend_arc =
                        self.backend_arc_for(info.format)
                            .ok_or(AiEngineError::ModelLoadError(format!(
                                "no backend for format {:?}",
                                info.format
                            )))?;
                    return router
                        .submit(key, info.id, input, backend_arc, Path::new(&info.path))
                        .await;
                }
            }

            backend.infer(info.id, input).await
        }

        /// Look up the backend for a given model format (public for Engine
        /// to query `supports_dma_input()` during preprocessing dispatch).
        pub fn backend_for_format(&self, format: ModelFormat) -> Option<&dyn ModelBackend> {
            self.backend_for(format)
        }
    }

    #[async_trait::async_trait]
    impl AiModelRegistry for ModelRegistry {
        async fn probe_model(&self, file_path: &Path) -> Result<ModelProbeInfo, AiEngineError> {
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

            let prober_registry = Arc::clone(&self.prober_registry);
            let ext_owned = ext.to_string();
            let path = file_path.to_path_buf();

            tokio::task::spawn_blocking(move || {
                let prober = prober_registry.find_by_extension(&ext_owned).ok_or(
                    AiEngineError::ModelLoadError(format!(
                        "unsupported model format: .{ext_owned}"
                    )),
                )?;
                prober.probe(&path)
            })
            .await
            .map_err(|e| AiEngineError::ModelLoadError(format!("probe join error: {e}")))?
        }

        async fn install_model(
            &self,
            file_path: &Path,
            user_meta: ModelInstallRequest,
        ) -> Result<ModelInfo, AiEngineError> {
            use ng_gateway_models::entities::ai::model::{Labels, TensorDescs};

            // 1. Probe
            let probe_info = self.probe_model(file_path).await?;

            // 2. Derive key from filename stem
            let file_stem = file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("model");
            let key = user_meta
                .name
                .as_deref()
                .map(|n| n.to_lowercase().replace(' ', "-"))
                .unwrap_or(file_stem.to_string());
            let ext = file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("onnx");
            let dest_filename = format!("{key}.{ext}");
            let dest_path = self.models_dir.join(&dest_filename);

            // 3. Determine task from probe or user override
            let task = user_meta
                .task
                .or(probe_info.inferred_task)
                .unwrap_or(ng_gateway_models::enums::ai::ModelTask::ObjectDetection);

            // 4. Build labels from probe or user override
            let labels = user_meta
                .labels
                .map(Labels)
                .or(probe_info.labels.map(Labels));

            // 5. Build NewModel
            let new_model = NewModel {
                key: key.clone(),
                name: user_meta.name.unwrap_or(file_stem.to_string()),
                version: user_meta.version.unwrap_or("1.0.0".into()),
                task,
                format: probe_info.format,
                path: dest_path.to_string_lossy().to_string(),
                labels,
                default_preprocess: user_meta
                    .default_preprocess
                    .or(probe_info.recommended_preprocess),
                default_postprocess: user_meta.default_postprocess.or(probe_info
                    .recommended_postprocessor
                    .map(
                        |t| ng_gateway_models::entities::ai::pipeline::PostProcessorConfig {
                            r#type: Some(t),
                            top_k: None,
                            apply_softmax: None,
                            max_detections: None,
                            num_keypoints: None,
                            anomaly_threshold: None,
                            nms_variant: None,
                            soft_nms_sigma: None,
                            detection_parallel_threshold: None,
                            nms_prescreen_multiplier: None,
                            classification_small_class_fast_path: None,
                            segmentation_parallel_min_pixels: None,
                        },
                    )),
                inputs: TensorDescs(probe_info.inputs),
                outputs: TensorDescs(probe_info.outputs),
                size: probe_info.size,
                checksum: probe_info.checksum.clone(),
            };

            let entity = ModelRepository::create(
                new_model.into_active_model(),
                None::<&sea_orm::DatabaseConnection>,
            )
            .await
            .map_err(|e| AiEngineError::IoError(format!("DB insert: {e}")))?;

            // 6. Copy artifact to models directory
            if let Err(e) = tokio::fs::copy(file_path, &dest_path).await {
                let _ =
                    ModelRepository::delete_by_key::<sea_orm::DatabaseConnection>(&key, None).await;
                return Err(AiEngineError::IoError(format!("copy model file: {e}")));
            }

            // 7. Cache + reverse index
            let info = ModelInfo::from(entity);
            self.key_index.insert(info.key.clone(), info.id);
            self.cache.insert(info.id, Arc::new(info.clone()));

            info!(
                model_id = info.id,
                key = %info.key,
                format = ?info.format,
                "model installed"
            );
            Ok(info)
        }

        async fn uninstall_model(&self, model_id: i32) -> Result<(), AiEngineError> {
            let info = self
                .get(model_id)
                .ok_or(AiEngineError::ModelNotFound(format!("model {model_id}")))?;

            // 1. Unload from backend if loaded
            if let Some(backend) = self.backend_for(info.format) {
                backend.unload(model_id);
            }

            // 2. Remove file (best-effort)
            let _ = tokio::fs::remove_file(&info.path).await;

            // 3. DB delete
            ModelRepository::delete_by_key::<sea_orm::DatabaseConnection>(&info.key, None)
                .await
                .map_err(|e| AiEngineError::IoError(format!("DB delete: {e}")))?;

            // 4. Evict cache + reverse index
            self.key_index.remove(&info.key);
            self.cache.remove(&model_id);

            info!(model_id, key = %info.key, "model uninstalled");
            Ok(())
        }

        async fn update_model(&self, model: UpdateModel) -> Result<ModelInfo, AiEngineError> {
            let model_id = model.id;
            let existing = self
                .get(model_id)
                .ok_or(AiEngineError::ModelNotFound(format!("model {model_id}")))?;

            let entity = ModelRepository::update(
                model.into_active_model(),
                None::<&sea_orm::DatabaseConnection>,
            )
            .await
            .map_err(|e| AiEngineError::IoError(format!("DB update: {e}")))?;

            let info = ModelInfo::from(entity);
            if existing.key != info.key {
                self.key_index.remove(&existing.key);
            }
            self.key_index.insert(info.key.clone(), info.id);
            self.cache.insert(info.id, Arc::new(info.clone()));
            Ok(info)
        }

        async fn load_model(&self, model_id: i32) -> Result<(), AiEngineError> {
            let info = self
                .get(model_id)
                .ok_or(AiEngineError::ModelNotFound(format!("model {model_id}")))?;

            let backend = self
                .backend_for(info.format)
                .ok_or(AiEngineError::ModelLoadError(format!(
                    "no backend for format {:?}",
                    info.format
                )))?;

            if backend.is_loaded(model_id) {
                return Ok(());
            }

            backend.load(model_id, Path::new(&info.path)).await?;
            info!(model_id, key = %info.key, "model loaded into backend");
            Ok(())
        }

        async fn unload_model(&self, model_id: i32) -> Result<(), AiEngineError> {
            let info = self
                .get(model_id)
                .ok_or(AiEngineError::ModelNotFound(format!("model {model_id}")))?;

            if let Some(backend) = self.backend_for(info.format) {
                backend.unload(model_id);
                info!(model_id, key = %info.key, "model unloaded from backend");
            }
            Ok(())
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, AiEngineError> {
            Ok(self.list_all().into_iter().map(|m| (*m).clone()).collect())
        }

        async fn get_model(&self, model_id: i32) -> Result<Option<ModelInfo>, AiEngineError> {
            Ok(self.get(model_id).map(|m| (*m).clone()))
        }

        async fn page_models(
            &self,
            params: ModelPageParams,
        ) -> Result<PageResult<ModelInfo>, AiEngineError> {
            ModelRepository::page(params)
                .await
                .map_err(|e| AiEngineError::IoError(e.to_string()))
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
