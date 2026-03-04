use crate::{
    entities::ai::alarm_rule::{
        ActiveModel, AlarmCondition, Entity as AlarmRuleEntity, Model as AlarmRuleModel,
    },
    enums::ai::AlarmSeverity,
    initializer::SeedableTrait,
};
use sea_orm::{prelude::DateTimeUtc, DeriveIntoActiveModel, FromQueryResult, IntoActiveModel};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Model metadata stored in the registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct AlarmRuleInfo {
    /// Alarm rule unique identifier.
    pub id: i32,
    /// Rule name.
    pub name: String,
    /// Pipeline ID.
    pub pipeline_id: i32,
    /// Rule order.
    pub rule_order: i32,
    /// Rule severity.
    pub severity: AlarmSeverity,
    /// Rule condition.
    pub condition: AlarmCondition,
    /// Rule cooldown seconds.
    pub cooldown_secs: u32,
    /// Rule minimum duration seconds.
    pub min_duration_secs: Option<u32>,
    /// Created at timestamp.
    pub created_at: DateTimeUtc,
    /// Updated at timestamp.
    pub updated_at: DateTimeUtc,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewAlarmRule {
    /// Rule name.
    pub name: String,
    /// Pipeline ID.
    pub pipeline_id: i32,
    /// Rule order.
    pub rule_order: i32,
    /// Rule severity.
    pub severity: AlarmSeverity,
    /// Rule condition.
    pub condition: AlarmCondition,
    /// Rule cooldown seconds.
    pub cooldown_secs: u32,
    /// Rule minimum duration seconds.
    pub min_duration_secs: Option<u32>,
}

impl SeedableTrait for NewAlarmRule {
    type ActiveModel = ActiveModel;
    type Entity = AlarmRuleEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAlarmRule {
    pub id: i32,
    pub name: String,
    pub rule_order: i32,
    pub severity: AlarmSeverity,
    pub condition: AlarmCondition,
    pub cooldown_secs: u32,
    pub min_duration_secs: Option<Option<u32>>,
}

impl AlarmRuleInfo {
    /// Build an active model ready for DB insertion as part of a pipeline
    /// create/update operation. Caller provides the owning `pipeline_id`
    /// and the positional `order` within the pipeline.
    pub fn to_insert_active_model(&self, pipeline_id: i32, order: i32) -> ActiveModel {
        NewAlarmRule {
            name: self.name.clone(),
            pipeline_id,
            rule_order: order,
            severity: self.severity,
            condition: self.condition.clone(),
            cooldown_secs: self.cooldown_secs,
            min_duration_secs: self.min_duration_secs,
        }
        .into_active_model()
    }
}

impl From<AlarmRuleModel> for AlarmRuleInfo {
    fn from(model: AlarmRuleModel) -> Self {
        Self {
            id: model.id,
            name: model.name,
            pipeline_id: model.pipeline_id,
            rule_order: model.rule_order,
            severity: model.severity,
            condition: model.condition,
            cooldown_secs: model.cooldown_secs,
            min_duration_secs: model.min_duration_secs,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}
