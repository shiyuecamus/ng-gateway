//! Model registry — manages AI model lifecycle.
//!
//! Scans a directory for `.onnx` files at startup, extracts input/output
//! metadata via ONNX Runtime, and provides lazy loading on first use.

#[cfg(feature = "engine")]
mod inner {
    use dashmap::DashMap;
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::model::{
        ModelFormat, ModelInfo, ModelTask, ModelUpdateRequest, TensorDesc,
    };
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };
    use tracing::{info, warn};

    /// Model registry — manages AI model metadata and lazy loading.
    pub struct ModelRegistry {
        /// Model metadata cache keyed by model id.
        models: DashMap<String, Arc<ModelInfo>>,
        /// Root directory for model files.
        models_dir: PathBuf,
    }

    impl ModelRegistry {
        /// Get model info by ID (synchronous helper for hot registration paths).
        #[inline]
        pub fn get_shared(&self, model_id: &str) -> Option<Arc<ModelInfo>> {
            self.models.get(model_id).map(|r| Arc::clone(r.value()))
        }

        /// Create a new registry by scanning the models directory.
        ///
        /// Discovered `.onnx` files are probed for metadata. Models that
        /// fail to probe are logged as warnings and skipped.
        pub async fn new(models_dir: &Path) -> Result<Self, AiEngineError> {
            let registry = Self {
                models: DashMap::new(),
                models_dir: models_dir.to_path_buf(),
            };

            if models_dir.exists() {
                let mut entries = tokio::fs::read_dir(models_dir)
                    .await
                    .map_err(|e| AiEngineError::IoError(e.to_string()))?;

                while let Some(entry) = entries
                    .next_entry()
                    .await
                    .map_err(|e| AiEngineError::IoError(e.to_string()))?
                {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "onnx") {
                        match Self::probe_model(&path).await {
                            Ok(info) => {
                                info!(model_id = %info.id, path = %path.display(), "discovered AI model");
                                registry.models.insert(info.id.clone(), Arc::new(info));
                            }
                            Err(e) => {
                                warn!(path = %path.display(), error = %e, "failed to probe model");
                            }
                        }
                    }
                }
            } else {
                info!(dir = %models_dir.display(), "AI models directory does not exist, creating");
                tokio::fs::create_dir_all(models_dir)
                    .await
                    .map_err(|e| AiEngineError::IoError(e.to_string()))?;
            }

            info!(
                count = registry.models.len(),
                "AI model registry initialized"
            );
            Ok(registry)
        }

        /// Probe a model file to extract metadata.
        ///
        /// Phase 1: uses file-level metadata only. Full ONNX Runtime probing
        /// (input/output shapes) happens when the model is first loaded.
        async fn probe_model(path: &Path) -> Result<ModelInfo, AiEngineError> {
            let file_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            let metadata = tokio::fs::metadata(path)
                .await
                .map_err(|e| AiEngineError::IoError(e.to_string()))?;

            // Try to load a sidecar labels file: `<model_name>.labels.txt`
            let labels_path = path.with_extension("labels.txt");
            let labels = if labels_path.exists() {
                tokio::fs::read_to_string(&labels_path)
                    .await
                    .map(|s| {
                        s.lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // Try to load a sidecar config: `<model_name>.json`
            let config_path = path.with_extension("json");
            let (task, inputs, outputs) = if config_path.exists() {
                Self::parse_sidecar_config(&config_path).await.unwrap_or((
                    ModelTask::ObjectDetection,
                    Vec::new(),
                    Vec::new(),
                ))
            } else {
                (ModelTask::ObjectDetection, Vec::new(), Vec::new())
            };

            Ok(ModelInfo {
                id: file_name.clone(),
                name: file_name,
                version: "1.0.0".to_string(),
                format: ModelFormat::Onnx,
                path: path.to_path_buf(),
                inputs,
                outputs,
                task,
                labels,
                default_preprocess: None,
                default_postprocess: None,
                loaded: false,
                file_size: metadata.len(),
            })
        }

        /// Parse an optional sidecar JSON config for model metadata.
        ///
        /// Expected format:
        /// ```json
        /// {
        ///   "task": "object_detection",
        ///   "inputs": [{"name": "images", "shape": [1,3,640,640], "dtype": "float32"}],
        ///   "outputs": [{"name": "output0", "shape": [1,84,8400], "dtype": "float32"}]
        /// }
        /// ```
        async fn parse_sidecar_config(
            path: &Path,
        ) -> Result<(ModelTask, Vec<TensorDesc>, Vec<TensorDesc>), AiEngineError> {
            let content = tokio::fs::read_to_string(path)
                .await
                .map_err(|e| AiEngineError::IoError(e.to_string()))?;

            #[derive(serde::Deserialize)]
            struct SidecarConfig {
                #[serde(default)]
                task: Option<ModelTask>,
                #[serde(default)]
                inputs: Vec<TensorDesc>,
                #[serde(default)]
                outputs: Vec<TensorDesc>,
            }

            let cfg: SidecarConfig = serde_json::from_str(&content)
                .map_err(|e| AiEngineError::IoError(format!("sidecar config parse: {e}")))?;

            Ok((
                cfg.task.unwrap_or(ModelTask::ObjectDetection),
                cfg.inputs,
                cfg.outputs,
            ))
        }

        /// Get model info by ID.
        pub async fn get(&self, model_id: &str) -> Option<Arc<ModelInfo>> {
            self.get_shared(model_id)
        }

        /// List all registered models.
        pub async fn list_all(&self) -> Result<Vec<Arc<ModelInfo>>, AiEngineError> {
            Ok(self
                .models
                .iter()
                .map(|entry| Arc::clone(entry.value()))
                .collect())
        }

        /// Mark a model as loaded in the registry.
        pub fn mark_loaded(&self, model_id: &str) {
            if let Some(mut entry) = self.models.get_mut(model_id) {
                Arc::make_mut(entry.value_mut()).loaded = true;
            }
        }

        /// Mark a model as unloaded in the registry.
        pub fn mark_unloaded(&self, model_id: &str) {
            if let Some(mut entry) = self.models.get_mut(model_id) {
                Arc::make_mut(entry.value_mut()).loaded = false;
            }
        }

        /// Update model metadata (e.g., after ONNX Runtime probing reveals shapes).
        pub fn update_tensor_info(
            &self,
            model_id: &str,
            inputs: Vec<TensorDesc>,
            outputs: Vec<TensorDesc>,
        ) {
            if let Some(mut entry) = self.models.get_mut(model_id) {
                let model = Arc::make_mut(entry.value_mut());
                model.inputs = inputs;
                model.outputs = outputs;
            }
        }

        /// Insert or replace model metadata entry.
        pub fn upsert(&self, model_info: ModelInfo) {
            self.models
                .insert(model_info.id.clone(), Arc::new(model_info));
        }

        /// Insert or replace model metadata entry using shared ownership.
        pub fn upsert_shared(&self, model_info: Arc<ModelInfo>) {
            self.models
                .insert(model_info.id.clone(), Arc::clone(&model_info));
        }

        /// Remove model metadata by identifier.
        pub fn remove(&self, model_id: &str) -> Option<Arc<ModelInfo>> {
            self.models.remove(model_id).map(|(_, info)| info)
        }

        /// Update mutable model metadata fields from API update request.
        pub fn update_model_metadata(
            &self,
            model_id: &str,
            request: ModelUpdateRequest,
        ) -> Result<(), AiEngineError> {
            let mut entry = self
                .models
                .get_mut(model_id)
                .ok_or_else(|| AiEngineError::ModelNotFound(model_id.to_string()))?;

            if let Some(v) = request.name {
                Arc::make_mut(entry.value_mut()).name = v;
            }
            if let Some(v) = request.version {
                Arc::make_mut(entry.value_mut()).version = v;
            }
            if let Some(v) = request.task {
                Arc::make_mut(entry.value_mut()).task = v;
            }
            if let Some(v) = request.labels {
                Arc::make_mut(entry.value_mut()).labels = v;
            }
            if let Some(v) = request.default_preprocess {
                Arc::make_mut(entry.value_mut()).default_preprocess = Some(v);
            }
            if let Some(v) = request.default_postprocess {
                Arc::make_mut(entry.value_mut()).default_postprocess = Some(v);
            }
            Ok(())
        }

        /// Get the models directory path.
        pub fn models_dir(&self) -> &Path {
            &self.models_dir
        }
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
