//! Pipeline compiler — converts user pipeline config into a runtime plan.
//!
//! Compilation resolves model metadata and stage processors once at
//! registration time so the per-frame hot path can execute with pre-bound
//! handles and immutable stage payload.

#[cfg(feature = "engine")]
mod inner {
    use crate::{
        model::{profile::resolve_stage_processors, registry::ModelRegistry},
        pipeline::{postprocess::PostProcessor, preprocess::PreProcessor},
    };
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::{
        domain::prelude::{AlarmRuleInfo, ModelInfo, NewPipeline},
        entities::ai::{
            pipeline::{AnnotationConfig, RegionOfInterest},
            pipeline_stage::StageConfig,
        },
        enums::ai::{SamplingStrategy, TensorDType, TrackerAlgorithm},
    };
    use std::sync::Arc;

    /// Opaque model handle used by compiled inference stages.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ModelHandle(pub usize);

    /// Opaque stage handle used by runtime diagnostics.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct StageHandle(pub usize);

    /// Compiled metadata needed for one loaded model.
    #[derive(Debug)]
    pub struct CompiledModel {
        /// Stable runtime handle.
        pub handle: ModelHandle,
        /// Shared immutable model metadata.
        pub info: Arc<ModelInfo>,
        /// Default model input shape cached at compile time.
        pub default_input_shape: Arc<[i64]>,
        /// Default model input dtype cached at compile time.
        pub default_input_dtype: TensorDType,
    }

    /// Compiled inference stage with pre-bound processors.
    pub struct CompiledInferenceStage {
        /// Stage handle in compiled stage array.
        pub stage: StageHandle,
        /// Referenced model handle.
        pub model: ModelHandle,
        /// Effective model input shape for this stage.
        pub input_shape: Arc<[i64]>,
        /// Effective model input dtype for this stage.
        pub input_dtype: TensorDType,
        /// Pre-bound preprocessor instance.
        pub preprocessor: Arc<dyn PreProcessor>,
        /// Pre-bound postprocessor instance.
        pub postprocessor: Arc<dyn PostProcessor>,
    }

    /// One runtime-executable compiled stage.
    pub enum CompiledStage {
        /// Built-in model inference stage.
        Inference(CompiledInferenceStage),
        /// Built-in tracker stage.
        Tracker {
            /// Stage handle in compiled stage array.
            stage: StageHandle,
            /// Tracker algorithm configuration.
            algorithm: TrackerAlgorithm,
            /// Maximum stale frame age for tracks.
            max_age: u32,
        },
        /// WASM frame transform stage.
        FrameTransform {
            /// Stage handle in compiled stage array.
            stage: StageHandle,
            /// Target module id.
            module_id: Arc<str>,
            /// Immutable JSON config payload.
            config: Arc<serde_json::Value>,
        },
        /// WASM result processor stage.
        ResultProcessor {
            /// Stage handle in compiled stage array.
            stage: StageHandle,
            /// Target module id.
            module_id: Arc<str>,
            /// Immutable JSON config payload.
            config: Arc<serde_json::Value>,
        },
    }

    /// Runtime-optimized pipeline representation.
    pub struct CompiledPipeline {
        /// Pipeline name.
        pub name: Arc<str>,
        /// Sampling policy.
        pub sampling: SamplingStrategy,
        /// Optional single ROI.
        pub roi: Option<RegionOfInterest>,
        /// Optional multi-ROI list.
        pub roi_regions: Arc<[RegionOfInterest]>,
        /// Ordered compiled stages.
        pub stages: Arc<[CompiledStage]>,
        /// Alarm rules.
        pub alarm_rules: Arc<[AlarmRuleInfo]>,
        /// Pre-compiled annotation config shared across frames (avoids per-frame clone).
        pub annotation: Arc<AnnotationConfig>,
        /// Referenced compiled model table.
        pub models: Arc<[CompiledModel]>,
    }

    impl CompiledPipeline {
        /// Look up a compiled model by handle.
        #[inline]
        pub fn model(&self, handle: ModelHandle) -> Option<&CompiledModel> {
            self.models.get(handle.0)
        }
    }

    /// Compile a user pipeline into a runtime-optimized immutable plan.
    pub fn compile_pipeline(
        config: &NewPipeline,
        model_registry: &ModelRegistry,
    ) -> Result<CompiledPipeline, AiEngineError> {
        let mut models: Vec<CompiledModel> = Vec::new();
        let mut stages: Vec<CompiledStage> = Vec::with_capacity(config.stages.len());

        for (stage_index, stage_info) in config.stages.iter().enumerate() {
            let stage_handle = StageHandle(stage_index);
            match &stage_info.config {
                StageConfig::Inference {
                    model_id,
                    confidence_threshold,
                    nms_iou_threshold,
                    input_size,
                    preprocess: pre_cfg,
                    postprocess: post_cfg,
                } => {
                    let model_info = model_registry
                        .get_by_key(model_id)
                        .ok_or(AiEngineError::ModelNotFound(model_id.to_string()))?;
                    let model_handle = push_or_get_model_handle(&mut models, model_info);
                    let model = models
                        .get(model_handle.0)
                        .ok_or(AiEngineError::InternalError(
                            "compiled model handle out of bounds".into(),
                        ))?;
                    let (preprocessor, postprocessor) = resolve_stage_processors(
                        model.info.as_ref(),
                        *confidence_threshold,
                        *nms_iou_threshold,
                        pre_cfg.as_deref(),
                        post_cfg.as_deref(),
                    )?;
                    let input_shape: Arc<[i64]> = input_size
                        .map(|(w, h)| Arc::from([1_i64, 3, h as i64, w as i64]))
                        .unwrap_or(Arc::clone(&model.default_input_shape));
                    stages.push(CompiledStage::Inference(CompiledInferenceStage {
                        stage: stage_handle,
                        model: model_handle,
                        input_shape,
                        input_dtype: model.default_input_dtype,
                        preprocessor,
                        postprocessor,
                    }));
                }
                StageConfig::Tracker { algorithm, max_age } => {
                    stages.push(CompiledStage::Tracker {
                        stage: stage_handle,
                        algorithm: algorithm.clone(),
                        max_age: *max_age,
                    });
                }
                StageConfig::FrameTransform { module_id, config } => {
                    stages.push(CompiledStage::FrameTransform {
                        stage: stage_handle,
                        module_id: Arc::<str>::from(module_id.as_str()),
                        config: Arc::new(config.clone()),
                    });
                }
                StageConfig::ResultProcessor { module_id, config } => {
                    stages.push(CompiledStage::ResultProcessor {
                        stage: stage_handle,
                        module_id: Arc::<str>::from(module_id.as_str()),
                        config: Arc::new(config.clone()),
                    });
                }
            }
        }

        Ok(CompiledPipeline {
            name: Arc::<str>::from(config.name.as_str()),
            sampling: config.sampling.clone(),
            roi: None,
            roi_regions: config.roi_regions.0.clone().into(),
            stages: stages.into(),
            alarm_rules: config.alarm_rules.clone().into(),
            annotation: Arc::new(config.annotation.clone()),
            models: models.into(),
        })
    }

    fn push_or_get_model_handle(
        models: &mut Vec<CompiledModel>,
        info: Arc<ModelInfo>,
    ) -> ModelHandle {
        if let Some((idx, _)) = models
            .iter()
            .enumerate()
            .find(|(_, compiled)| compiled.info.id == info.id)
        {
            return ModelHandle(idx);
        }

        let default_input_shape: Arc<[i64]> = info
            .inputs
            .as_ref()
            .and_then(|inputs| inputs.0.first())
            .map(|input| input.shape.clone().into())
            .unwrap_or(Arc::from([1_i64, 3, 640, 640]));
        let default_input_dtype = info
            .inputs
            .as_ref()
            .and_then(|inputs| inputs.0.first())
            .map(|input| input.dtype)
            .unwrap_or(TensorDType::Float32);
        let handle = ModelHandle(models.len());
        models.push(CompiledModel {
            handle,
            info,
            default_input_shape,
            default_input_dtype,
        });
        handle
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
