//! `SeaORM` entity for `pipeline_stage`.

use crate::{
    entities::ai::pipeline::{PostProcessorConfig, PreProcessorConfig},
    enums::ai::TrackerAlgorithm,
};
use ng_gateway_macros::IntoActiveValue;
use sea_orm::{entity::prelude::*, FromJsonQueryResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "pipeline_stage")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub pipeline_id: i32,
    pub stage_order: i32,
    pub config: StageConfig,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

/// A single processing stage in the pipeline.
///
/// Stages are executed in order. The pipeline enforces:
/// - `FrameTransform` must come before `Inference`
/// - `Tracker` must follow an `Inference`
/// - `ResultProcessor` must come after `Inference`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, IntoActiveValue, FromJsonQueryResult)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StageConfig {
    /// Custom WASM frame-level transform (before inference).
    FrameTransform {
        /// WASM module identifier.
        module_id: String,
        /// JSON configuration passed to the WASM module.
        #[serde(default)]
        config: serde_json::Value,
    },

    /// Built-in model inference stage.
    ///
    /// Internally chains: PreProcessor(config) → ONNX Runtime → PostProcessor(config).
    /// Users only configure parameters; processor selection is automatic based on
    /// model task + output shape, or manually overridden via config.
    Inference {
        /// Model identifier in the registry.
        model_id: String,
        /// Confidence threshold for filtering results.
        #[serde(default = "StageConfig::default_confidence_threshold")]
        confidence_threshold: f32,
        /// NMS IoU threshold (for detection models).
        nms_iou_threshold: Option<f32>,
        /// Target input size override (uses model metadata if not set).
        input_size: Option<(u32, u32)>,
        /// Preprocessing configuration override (optional).
        /// Boxed to keep enum variant size small (see clippy::large_enum_variant).
        preprocess: Option<Box<PreProcessorConfig>>,
        /// Postprocessing configuration override (optional).
        /// Boxed to keep enum variant size small (see clippy::large_enum_variant).
        postprocess: Option<Box<PostProcessorConfig>>,
    },

    /// Built-in object tracker stage (applied after detection).
    Tracker {
        /// Tracker algorithm.
        algorithm: TrackerAlgorithm,
        /// Maximum age (frames) before dropping a track.
        #[serde(default = "StageConfig::default_tracker_max_age")]
        max_age: u32,
    },

    /// Custom WASM result-level processor (after inference/tracker).
    ResultProcessor {
        /// WASM module identifier.
        module_id: String,
        /// JSON configuration passed to the WASM module.
        #[serde(default)]
        config: serde_json::Value,
    },
}

impl StageConfig {
    pub fn default_confidence_threshold() -> f32 {
        0.5
    }

    pub fn default_tracker_max_age() -> u32 {
        100
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::pipeline::Entity",
        from = "Column::PipelineId",
        to = "super::pipeline::Column::Id"
    )]
    Pipeline,
}

impl Related<super::pipeline::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Pipeline.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
