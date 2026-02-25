//! AI-related domain response types.

use crate::ai::pipeline::PipelineConfig;
use serde::Serialize;

/// Pipeline summary returned by the pipeline list endpoint.
///
/// Wraps a `PipelineConfig` with its bound channel ID for the
/// `GET /api/ai/pipelines` response.
#[derive(Debug, Clone, Serialize)]
pub struct AiPipelineSummary {
    pub channel_id: i32,
    #[serde(flatten)]
    pub config: PipelineConfig,
}
