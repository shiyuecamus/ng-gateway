//! Pipeline registry — three-layer management of pipeline definitions,
//! bindings, and runtime compiled pipelines.
//!
//! ## Three Layers
//!
//! 1. **Definition** (DB: `pipeline` + `pipeline_stage` + `alarm_rule`)
//!    The blueprint with ordered stages and alarm rules.
//!
//! 2. **Binding** (DB: `pipeline_binding`)
//!    Which channel uses which pipeline definition.
//!
//! 3. **Runtime** (in-memory: `CompiledPipeline` + `TrackerRuntime`)
//!    The compiled, optimized form executing inference per-frame.
//!
//! Mutation flow: DB write → cache update → re-compile affected bindings.

use crate::{
    model::registry::ModelRegistry,
    pipeline::{
        compiled::{compile_pipeline, CompiledPipeline},
        tracker::TrackerRuntime,
    },
};
use dashmap::DashMap;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{
        AlarmRuleInfo, NewPipeline, PageResult, PipelineInfo, PipelinePageParams,
        PipelineStageInfo, UpdatePipeline,
    },
    enums::common::Status,
    AiPipelineRegistry,
};
use ng_gateway_repository::{
    AlarmRuleRepository, PipelineBindingRepository, PipelineRepository, PipelineStageRepository,
};
use sea_orm::{DatabaseConnection, IntoActiveModel};
use std::sync::Arc;
use tracing::{info, warn};

/// Convenience conversion from storage errors to AI engine errors.
#[inline]
fn storage_err(context: &str, e: impl std::fmt::Display) -> AiEngineError {
    AiEngineError::IoError(format!("{context}: {e}"))
}

/// Runtime binding for an active channel → compiled pipeline.
pub struct ActiveBinding {
    /// Original pipeline definition info.
    pub info: Arc<PipelineInfo>,
    /// Runtime-optimized compiled pipeline.
    pub compiled: Arc<CompiledPipeline>,
}

/// Pipeline registry — DB-backed write-through cache with three layers.
pub struct PipelineRegistry {
    /// Pipeline definitions cache keyed by pipeline id.
    definitions: DashMap<i32, Arc<PipelineInfo>>,
    /// Active runtime bindings keyed by channel_id.
    bindings: DashMap<i32, Arc<ActiveBinding>>,
    /// Per-channel tracker runtimes (for `CompiledStage::Tracker`).
    pub tracker_runtimes: Arc<DashMap<i32, TrackerRuntime>>,
    /// Model registry reference for pipeline compilation.
    model_registry: Arc<ModelRegistry>,
}

impl PipelineRegistry {
    /// Initialize from DB records.
    ///
    /// Hydrates all pipeline definitions and restores active bindings
    /// using batch-loaded relation queries (3 queries total, no N+1).
    ///
    /// `db_conn` provides an externally-owned database connection for use
    /// during gateway startup before `NGAppContext` is initialized. When
    /// `None`, the repository falls back to `NGAppContext` (which must
    /// already be set).
    pub async fn new(
        model_registry: Arc<ModelRegistry>,
        db_conn: Option<&DatabaseConnection>,
    ) -> Result<Self, AiEngineError> {
        let registry = Self {
            definitions: DashMap::new(),
            bindings: DashMap::new(),
            tracker_runtimes: Arc::new(DashMap::new()),
            model_registry,
        };

        // Hydrate pipeline definitions with batch-loaded relations.
        let all_pipelines = PipelineRepository::list_all_with_relations(db_conn)
            .await
            .map_err(|e| storage_err("load pipeline definitions", e))?;

        for (entity, stages, rules) in all_pipelines {
            let info = PipelineInfo::with_relations(entity, stages, rules);
            registry.definitions.insert(info.id, Arc::new(info));
        }

        // Restore active bindings.
        let active_bindings = PipelineBindingRepository::list_enabled(db_conn)
            .await
            .map_err(|e| storage_err("load active bindings", e))?;

        for binding in active_bindings {
            if let Some(pipeline_info) = registry.definitions.get(&binding.pipeline_id) {
                let config = NewPipeline::from(pipeline_info.value().as_ref().clone());
                match compile_pipeline(&config, registry.model_registry.as_ref()) {
                    Ok(compiled) => {
                        registry.bindings.insert(
                            binding.channel_id,
                            Arc::new(ActiveBinding {
                                info: Arc::clone(pipeline_info.value()),
                                compiled: Arc::new(compiled),
                            }),
                        );
                    }
                    Err(e) => {
                        warn!(
                            channel_id = binding.channel_id,
                            pipeline_id = binding.pipeline_id,
                            error = %e,
                            "failed to compile pipeline binding on startup, skipping"
                        );
                    }
                }
            }
        }

        info!(
            definitions = registry.definitions.len(),
            active_bindings = registry.bindings.len(),
            "pipeline registry initialized from DB"
        );
        Ok(registry)
    }

    /// Get an active compiled binding for a channel (used by inference hot path).
    pub fn get_active_binding(&self, channel_id: i32) -> Option<Arc<ActiveBinding>> {
        self.bindings
            .get(&channel_id)
            .map(|e| Arc::clone(e.value()))
    }

    /// Number of active channel bindings.
    pub fn active_binding_count(&self) -> usize {
        self.bindings.len()
    }

    /// Number of pipeline definitions.
    pub fn definition_count(&self) -> usize {
        self.definitions.len()
    }

    /// Reload a definition from DB into cache (after external mutation).
    async fn reload_definition(
        &self,
        pipeline_id: i32,
    ) -> Result<Arc<PipelineInfo>, AiEngineError> {
        let (entity, stages, rules) = PipelineRepository::find_by_id_with_relations(pipeline_id)
            .await
            .map_err(|e| storage_err("reload pipeline", e))?
            .ok_or(AiEngineError::PipelineNotFound(pipeline_id))?;

        let info = PipelineInfo::with_relations(entity, stages, rules);
        let arc_info = Arc::new(info);
        self.definitions.insert(pipeline_id, Arc::clone(&arc_info));
        Ok(arc_info)
    }

    /// Re-compile all bindings that reference a given pipeline definition.
    fn recompile_bindings_for_pipeline(&self, pipeline_id: i32) {
        for mut entry in self.bindings.iter_mut() {
            if entry.value().info.id != pipeline_id {
                continue;
            }
            let channel_id = *entry.key();
            if let Some(new_info) = self.definitions.get(&pipeline_id) {
                let config = NewPipeline::from(new_info.value().as_ref().clone());
                match compile_pipeline(&config, self.model_registry.as_ref()) {
                    Ok(compiled) => {
                        *entry.value_mut() = Arc::new(ActiveBinding {
                            info: Arc::clone(new_info.value()),
                            compiled: Arc::new(compiled),
                        });
                        self.tracker_runtimes.remove(&channel_id);
                    }
                    Err(e) => {
                        warn!(
                            channel_id,
                            pipeline_id,
                            error = %e,
                            "failed to re-compile pipeline binding after update"
                        );
                    }
                }
            }
        }
    }

    /// Insert stage active models for a pipeline. Skips if `stages` is empty.
    async fn insert_stages(
        pipeline_id: i32,
        stages: &[PipelineStageInfo],
    ) -> Result<(), AiEngineError> {
        if stages.is_empty() {
            return Ok(());
        }
        let actives: Vec<_> = stages
            .iter()
            .enumerate()
            .map(|(i, s)| s.to_insert_active_model(pipeline_id, i as i32))
            .collect();
        PipelineStageRepository::replace_by_pipeline_id(
            pipeline_id,
            actives,
            None::<&sea_orm::DatabaseConnection>,
        )
        .await
        .map_err(|e| storage_err("insert stages", e))
    }

    /// Insert alarm rule active models for a pipeline. Skips if `rules` is empty.
    async fn insert_alarm_rules(
        pipeline_id: i32,
        rules: &[AlarmRuleInfo],
    ) -> Result<(), AiEngineError> {
        if rules.is_empty() {
            return Ok(());
        }
        let actives: Vec<_> = rules
            .iter()
            .enumerate()
            .map(|(i, r)| r.to_insert_active_model(pipeline_id, i as i32))
            .collect();
        AlarmRuleRepository::replace_by_pipeline_id(
            pipeline_id,
            actives,
            None::<&sea_orm::DatabaseConnection>,
        )
        .await
        .map_err(|e| storage_err("insert alarm rules", e))
    }
}

#[async_trait::async_trait]
impl AiPipelineRegistry for PipelineRegistry {
    async fn create_pipeline(&self, pipeline: NewPipeline) -> Result<PipelineInfo, AiEngineError> {
        // 1. Validate compilation eagerness (fail fast before touching DB).
        let _ = compile_pipeline(&pipeline, self.model_registry.as_ref())?;

        // 2. DB insert pipeline entity via DeriveIntoActiveModel.
        let stages = pipeline.stages.clone();
        let alarm_rules = pipeline.alarm_rules.clone();
        let active = pipeline.into_active_model();
        let entity = PipelineRepository::create(active, None::<&sea_orm::DatabaseConnection>)
            .await
            .map_err(|e| storage_err("DB insert pipeline", e))?;
        let pipeline_id = entity.id;

        // 3. DB insert stages.
        Self::insert_stages(pipeline_id, &stages).await?;

        // 4. DB insert alarm rules.
        Self::insert_alarm_rules(pipeline_id, &alarm_rules).await?;

        // 5. Reload full definition into cache.
        let info = self.reload_definition(pipeline_id).await?;
        info!(pipeline_id, pipeline_name = %info.name, "pipeline created");
        Ok(info.as_ref().clone())
    }

    async fn update_pipeline(&self, update: UpdatePipeline) -> Result<PipelineInfo, AiEngineError> {
        let pipeline_id = update.id;

        // 1. Update pipeline entity via DeriveIntoActiveModel (full replace).
        let stages = update.stages.clone();
        let alarm_rules = update.alarm_rules.clone();
        let active = update.into_active_model();
        PipelineRepository::update(active, None::<&sea_orm::DatabaseConnection>)
            .await
            .map_err(|e| storage_err("DB update pipeline", e))?;

        // 2. Replace stages.
        Self::insert_stages(pipeline_id, &stages).await?;

        // 3. Replace alarm rules.
        Self::insert_alarm_rules(pipeline_id, &alarm_rules).await?;

        // 4. Reload definition and re-compile affected bindings.
        let info = self.reload_definition(pipeline_id).await?;
        self.recompile_bindings_for_pipeline(pipeline_id);

        info!(pipeline_id, "pipeline updated, bindings re-compiled");
        Ok(info.as_ref().clone())
    }

    async fn delete_pipeline(&self, pipeline_id: i32) -> Result<(), AiEngineError> {
        // 1. Remove all bindings for this pipeline.
        let channels_to_unbind: Vec<i32> = self
            .bindings
            .iter()
            .filter(|e| e.value().info.id == pipeline_id)
            .map(|e| *e.key())
            .collect();

        for channel_id in channels_to_unbind {
            self.bindings.remove(&channel_id);
            self.tracker_runtimes.remove(&channel_id);
        }

        // 2. DB delete binding/stages/rules/pipeline.
        PipelineBindingRepository::delete_by_pipeline_id(pipeline_id)
            .await
            .map_err(|e| storage_err("DB delete bindings", e))?;
        PipelineStageRepository::delete_by_pipeline_id(pipeline_id)
            .await
            .map_err(|e| storage_err("DB delete stages", e))?;
        AlarmRuleRepository::delete_by_pipeline_id(pipeline_id)
            .await
            .map_err(|e| storage_err("DB delete alarm rules", e))?;
        PipelineRepository::delete_by_id(pipeline_id)
            .await
            .map_err(|e| storage_err("DB delete pipeline", e))?;

        // 3. Evict cache.
        self.definitions.remove(&pipeline_id);

        info!(pipeline_id, "pipeline deleted");
        Ok(())
    }

    async fn get_pipeline(&self, pipeline_id: i32) -> Result<Option<PipelineInfo>, AiEngineError> {
        Ok(self
            .definitions
            .get(&pipeline_id)
            .map(|e| e.value().as_ref().clone()))
    }

    async fn list_pipelines(&self) -> Result<Vec<PipelineInfo>, AiEngineError> {
        Ok(self
            .definitions
            .iter()
            .map(|e| e.value().as_ref().clone())
            .collect())
    }

    async fn page_pipelines(
        &self,
        params: PipelinePageParams,
    ) -> Result<PageResult<PipelineInfo>, AiEngineError> {
        PipelineRepository::page(params)
            .await
            .map_err(|e| storage_err("page pipelines", e))
    }

    async fn bind_pipeline(&self, channel_id: i32, pipeline_id: i32) -> Result<(), AiEngineError> {
        let pipeline_info = self
            .definitions
            .get(&pipeline_id)
            .map(|e| Arc::clone(e.value()))
            .ok_or(AiEngineError::PipelineNotFound(pipeline_id))?;

        // Compile.
        let config = NewPipeline::from(pipeline_info.as_ref().clone());
        let compiled = compile_pipeline(&config, self.model_registry.as_ref())?;

        // DB upsert binding.
        PipelineBindingRepository::upsert(channel_id, pipeline_id, Status::Enabled)
            .await
            .map_err(|e| storage_err("DB upsert binding", e))?;

        // Activate.
        self.bindings.insert(
            channel_id,
            Arc::new(ActiveBinding {
                info: pipeline_info,
                compiled: Arc::new(compiled),
            }),
        );
        self.tracker_runtimes.remove(&channel_id);

        info!(channel_id, pipeline_id, "pipeline bound to channel");
        Ok(())
    }

    async fn unbind_pipeline(&self, channel_id: i32) -> Result<(), AiEngineError> {
        self.bindings.remove(&channel_id);
        self.tracker_runtimes.remove(&channel_id);

        PipelineBindingRepository::delete_by_channel_id(channel_id)
            .await
            .map_err(|e| storage_err("DB delete binding", e))?;

        info!(channel_id, "pipeline unbound from channel");
        Ok(())
    }

    fn get_channel_pipeline(&self, channel_id: i32) -> Option<PipelineInfo> {
        self.bindings
            .get(&channel_id)
            .map(|e| e.value().info.as_ref().clone())
    }
}
