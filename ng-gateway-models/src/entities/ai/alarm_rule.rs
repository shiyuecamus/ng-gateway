//! `SeaORM` entity for `alarm_rule`.

use crate::{
    entities::NGEntity,
    enums::{
        ai::{AlarmSeverity, CrossingDirection},
        common::{EntityType, Status},
    },
};
use ng_gateway_macros::IntoActiveValue;
use sea_orm::{entity::prelude::*, FromJsonQueryResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "alarm_rule")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub pipeline_id: i32,
    pub rule_order: i32,
    pub name: String,
    pub severity: AlarmSeverity,
    pub condition: AlarmCondition,
    pub cooldown_secs: u32,
    pub min_duration_secs: Option<u32>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

/// Alarm triggering condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromJsonQueryResult, IntoActiveValue)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlarmCondition {
    /// Trigger when a specific class is detected.
    ClassDetected {
        class: String,
        #[serde(default = "AlarmCondition::default_confidence_threshold")]
        min_confidence: f32,
    },
    /// Trigger when object count exceeds threshold.
    CountExceeds {
        class: Option<String>,
        threshold: u32,
    },
    /// Trigger when an object enters a zone (polygon vertices).
    ZoneIntrusion {
        zone: Vec<(f32, f32)>,
        class: Option<String>,
    },
    /// Trigger when an object's track crosses a defined line segment.
    ///
    /// Requires a Tracker stage in the pipeline to provide trajectory data.
    /// The crossing direction can be constrained (e.g., only left-to-right).
    LineCrossing {
        /// Line segment: start point `(x1, y1)` and end point `(x2, y2)`.
        /// Coordinates are normalized `[0.0, 1.0]`.
        line: [(f32, f32); 2],
        /// Optional class filter.
        class: Option<String>,
        /// Crossing direction constraint (if `None`, any direction triggers).
        direction: Option<CrossingDirection>,
    },
    /// Trigger when an anomaly detection score exceeds a threshold.
    AnomalyDetected {
        /// Anomaly score threshold `[0.0, 1.0]`.
        #[serde(default = "AlarmCondition::default_confidence_threshold")]
        min_score: f32,
    },
    /// Trigger based on custom WASM evaluator.
    CustomWasm {
        module_id: String,
        #[serde(default)]
        config: serde_json::Value,
    },
}

impl AlarmCondition {
    fn default_confidence_threshold() -> f32 {
        0.5
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

impl NGEntity for Model {
    fn entity_type(&self) -> EntityType {
        EntityType::AlarmRule
    }

    fn id(&self) -> Option<i32> {
        Some(self.id)
    }

    fn status(&self) -> Option<Status> {
        None
    }
}

impl NGEntity for ActiveModel {
    fn entity_type(&self) -> EntityType {
        EntityType::AlarmRule
    }

    fn id(&self) -> Option<i32> {
        self.id.to_owned().take()
    }

    fn status(&self) -> Option<Status> {
        None
    }
}

impl ActiveModelBehavior for ActiveModel {}
