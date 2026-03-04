use crate::{
    domain::ai::{alarm_rule::AlarmRuleInfo, pipeline_stage::PipelineStageInfo},
    domain::common::{PageParams, TimeRangeParams},
    entities::{
        ai::{
            alarm_rule::{AlarmCondition, Model as AlarmRuleModel},
            pipeline::{ActiveModel, AnnotationConfig, Model as PipelineModel, RoiRegions},
            pipeline_stage::Model as PipelineStageModel,
        },
        prelude::StageConfig,
    },
    enums::{ai::SamplingStrategy, common::Status},
};
use sea_orm::{
    prelude::DateTimeUtc, DeriveIntoActiveModel, DerivePartialModel, FromQueryResult,
    IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Model metadata stored in the registry.
#[derive(Debug, Clone, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::PipelineModel as ModelTrait>::Entity")]
pub struct PipelineInfo {
    /// Pipeline unique identifier.
    pub id: i32,
    /// Pipeline key.
    pub key: String,
    /// Human-readable name.
    pub name: String,
    /// Frame sampling strategy.
    pub sampling: SamplingStrategy,
    /// Optional ROI (applied before inference).
    pub roi_regions: RoiRegions,
    /// Annotation rendering configuration.
    pub annotation: AnnotationConfig,
    /// Status.
    pub status: Status,
    /// Revision.
    pub revision: u32,
    /// Ordered list of processing stages.
    #[sea_orm(skip)]
    pub stages: Vec<PipelineStageInfo>,
    /// Alarm rules (post-processing triggers).
    #[sea_orm(skip)]
    pub alarm_rules: Vec<AlarmRuleInfo>,
    /// Created at timestamp.
    pub created_at: DateTimeUtc,
    /// Updated at timestamp.
    pub updated_at: DateTimeUtc,
}

impl PipelineInfo {
    /// Assemble from entity model with pre-loaded relations.
    ///
    /// Converts the raw entity models into domain info types. This avoids
    /// the N+1 query pattern — the caller pre-loads stages and rules in
    /// batch, then stitches them together here.
    pub fn with_relations(
        model: PipelineModel,
        stages: Vec<PipelineStageModel>,
        rules: Vec<AlarmRuleModel>,
    ) -> Self {
        Self {
            id: model.id,
            key: model.key,
            name: model.name,
            sampling: model.sampling,
            roi_regions: model.roi_regions,
            annotation: model.annotation,
            status: model.status,
            revision: model.revision,
            stages: stages.into_iter().map(Into::into).collect(),
            alarm_rules: rules.into_iter().map(Into::into).collect(),
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }

    /// Validate pipeline stage ordering and DAG-like constraints.
    ///
    /// Rules:
    /// - At least one `Inference` stage is required.
    /// - `FrameTransform` must be before any `Inference`.
    /// - `Tracker` must appear after `Inference` and at most once.
    /// - `ResultProcessor` must appear after `Inference`.
    /// - `Inference` cannot appear after `Tracker`/`ResultProcessor`.
    /// - `AlarmCondition::LineCrossing` requires a `Tracker` stage.
    pub fn validate_dag(&self) -> PipelineValidationReport {
        let mut report = PipelineValidationReport::ok();

        for (idx, roi) in self.roi_regions.0.iter().enumerate() {
            if !roi.is_valid() {
                report.push_error(format!(
                  "pipeline.roi_regions[{idx}] is invalid (expected normalized [0,1] bounds with min < max)"
              ));
            }
        }

        if self.stages.is_empty() {
            report.push_warning(
                "pipeline has no stages; no AI inference will be executed".to_string(),
            );
        }

        let mut inference_count = 0usize;
        let mut has_seen_inference = false;
        let mut has_seen_tracker = false;
        let mut has_seen_result_processor = false;
        let mut tracker_count = 0usize;

        for (idx, stage) in self.stages.iter().enumerate() {
            let stage_no = idx + 1;
            match stage.config {
                StageConfig::FrameTransform { .. } => {
                    if has_seen_inference {
                        report.push_error(format!(
                          "stage #{stage_no}: frame_transform must appear before any inference stage"
                      ));
                    }
                    if has_seen_tracker {
                        report.push_error(format!(
                            "stage #{stage_no}: frame_transform cannot appear after tracker"
                        ));
                    }
                    if has_seen_result_processor {
                        report.push_error(format!(
                          "stage #{stage_no}: frame_transform cannot appear after result_processor"
                      ));
                    }
                }
                StageConfig::Inference { .. } => {
                    if has_seen_tracker {
                        report.push_error(format!(
                            "stage #{stage_no}: inference cannot appear after tracker"
                        ));
                    }
                    if has_seen_result_processor {
                        report.push_error(format!(
                            "stage #{stage_no}: inference cannot appear after result_processor"
                        ));
                    }
                    has_seen_inference = true;
                    inference_count += 1;
                }
                StageConfig::Tracker { .. } => {
                    if !has_seen_inference {
                        report.push_error(format!(
                          "stage #{stage_no}: tracker must appear after at least one inference stage"
                      ));
                    }
                    if has_seen_result_processor {
                        report.push_error(format!(
                            "stage #{stage_no}: tracker cannot appear after result_processor"
                        ));
                    }
                    tracker_count += 1;
                    if tracker_count > 1 {
                        report.push_error(format!(
                            "stage #{stage_no}: only one tracker stage is allowed per pipeline"
                        ));
                    }
                    has_seen_tracker = true;
                }
                StageConfig::ResultProcessor { .. } => {
                    if !has_seen_inference {
                        report.push_error(format!(
                          "stage #{stage_no}: result_processor must appear after at least one inference stage"
                      ));
                    }
                    has_seen_result_processor = true;
                }
            }
        }

        if inference_count == 0 {
            report.push_error("pipeline must contain at least one inference stage".to_string());
        }

        let has_line_crossing_alarm = self.alarm_rules.iter().any(|rule| {
            matches!(
                rule.condition,
                AlarmCondition::LineCrossing {
                    line: _,
                    class: _,
                    direction: _
                }
            )
        });
        if has_line_crossing_alarm && !has_seen_tracker {
            report.push_error(
                "line_crossing alarm requires a tracker stage in the pipeline".to_string(),
            );
        }

        report
    }
}

/// Payload to create a new pipeline definition.
///
/// The `stages` and `alarm_rules` fields live in separate relation tables
/// and are inserted independently — only the pipeline-table columns are
/// mapped to `ActiveModel` via the manual `IntoActiveModel` impl.
#[derive(Clone, Debug, PartialEq, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewPipeline {
    /// Pipeline key.
    pub key: String,
    /// Human-readable name.
    pub name: String,
    /// Frame sampling strategy.
    pub sampling: SamplingStrategy,
    /// Optional ROI (applied before inference).
    pub roi_regions: RoiRegions,
    /// Annotation rendering configuration.
    pub annotation: AnnotationConfig,
    /// Revision.
    pub revision: u32,
    /// Ordered list of processing stages (stored in `pipeline_stage` table).
    pub stages: Vec<PipelineStageInfo>,
    /// Alarm rules (stored in `alarm_rule` table).
    pub alarm_rules: Vec<AlarmRuleInfo>,
}

impl IntoActiveModel<ActiveModel> for NewPipeline {
    fn into_active_model(self) -> ActiveModel {
        use sea_orm::ActiveValue::*;
        ActiveModel {
            id: NotSet,
            key: Set(self.key),
            name: Set(self.name),
            sampling: Set(self.sampling),
            roi_regions: Set(self.roi_regions),
            annotation: Set(self.annotation),
            revision: Set(self.revision),
            status: Set(Status::Enabled),
            created_at: NotSet,
            updated_at: NotSet,
        }
    }
}

impl From<PipelineInfo> for NewPipeline {
    fn from(info: PipelineInfo) -> Self {
        Self {
            key: info.key,
            name: info.name,
            sampling: info.sampling,
            roi_regions: info.roi_regions,
            annotation: info.annotation,
            revision: info.revision,
            stages: info.stages,
            alarm_rules: info.alarm_rules,
        }
    }
}

/// Payload to fully update an existing pipeline definition.
///
/// All pipeline-table fields are replaced (full update, not partial patch).
/// Stages and alarm rules are replaced independently via relation tables.
#[derive(Clone, Debug, PartialEq, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePipeline {
    /// Pipeline unique identifier.
    pub id: i32,
    /// Pipeline key.
    pub key: String,
    /// Human-readable name.
    pub name: String,
    /// Frame sampling strategy.
    pub sampling: SamplingStrategy,
    /// Optional ROI (applied before inference).
    pub roi_regions: RoiRegions,
    /// Annotation rendering configuration.
    pub annotation: AnnotationConfig,
    /// Revision.
    pub revision: u32,
    /// Ordered list of processing stages (stored in `pipeline_stage` table).
    pub stages: Vec<PipelineStageInfo>,
    /// Alarm rules (stored in `alarm_rule` table).
    pub alarm_rules: Vec<AlarmRuleInfo>,
}

impl IntoActiveModel<ActiveModel> for UpdatePipeline {
    fn into_active_model(self) -> ActiveModel {
        use sea_orm::ActiveValue::*;
        ActiveModel {
            id: Set(self.id),
            key: Set(self.key),
            name: Set(self.name),
            sampling: Set(self.sampling),
            roi_regions: Set(self.roi_regions),
            annotation: Set(self.annotation),
            revision: Set(self.revision),
            status: NotSet,
            created_at: NotSet,
            updated_at: NotSet,
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
pub struct ChangePipelineStatus {
    pub id: i32,
    pub status: Status,
}

/// Query parameters for paginating pipeline records.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PipelinePageParams {
    /// Fuzzy filter by pipeline name.
    pub name: Option<String>,
    /// Exact filter by pipeline status.
    pub status: Option<Status>,
    /// Pagination controls.
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    /// Created-at range filter.
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// Pipeline validation result for DAG/order constraints.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipelineValidationReport {
    /// Whether the pipeline satisfies all mandatory constraints.
    pub valid: bool,
    /// Hard validation errors that must be fixed before execution.
    pub errors: Vec<String>,
    /// Non-blocking warnings that may impact behaviour or quality.
    pub warnings: Vec<String>,
}

impl PipelineValidationReport {
    /// Create a successful validation report.
    #[inline]
    pub fn ok() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[inline]
    fn push_error(&mut self, message: String) {
        self.valid = false;
        self.errors.push(message);
    }

    #[inline]
    fn push_warning(&mut self, message: String) {
        self.warnings.push(message);
    }
}
