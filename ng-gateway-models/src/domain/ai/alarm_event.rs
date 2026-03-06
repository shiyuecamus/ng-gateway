//! Alarm event domain types for REST API.

use crate::{
    domain::common::{PageParams, TimeRangeParams},
    entities::ai::alarm_event::ActiveModel,
    enums::ai::{AlarmEventStatus, AlarmSeverity, AlarmType},
};
use sea_orm::{DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, ModelTrait};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Alarm event info returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::AlarmEventModel as ModelTrait>::Entity")]
pub struct AlarmEventInfo {
    pub id: i32,
    pub channel_id: i32,
    pub pipeline_id: Option<i32>,
    pub alarm_type: AlarmType,
    pub severity: AlarmSeverity,
    pub description: String,
    pub payload: Option<serde_json::Value>,
    pub status: AlarmEventStatus,
    pub acked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Paginated query parameters for alarm events.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct AlarmEventPageParams {
    /// Filter by channel ID.
    pub channel_id: Option<i32>,
    /// Filter by pipeline ID.
    pub pipeline_id: Option<i32>,
    /// Filter by alarm type.
    pub alarm_type: Option<AlarmType>,
    /// Filter by severity.
    pub severity: Option<AlarmSeverity>,
    /// Filter by status.
    pub status: Option<AlarmEventStatus>,
    /// Pagination controls.
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    /// Created-at range filter.
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// Request to update alarm event status (ack / close).
#[derive(Debug, Clone, Deserialize, DeriveIntoActiveModel, Validate)]
pub struct ChangeAlarmEventStatus {
    pub id: i32,
    pub status: AlarmEventStatus,
}
