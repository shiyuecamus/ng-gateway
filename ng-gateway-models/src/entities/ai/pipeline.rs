//! `SeaORM` entity for `pipeline`.

use crate::{
    entities::NGEntity,
    enums::{
        ai::{
            AnnotationQueueOverflowStrategy, ChannelOrder, NmsVariantConfig, NormalizationPreset,
            PostProcessorType, ResizeMode, SamplingStrategy,
        },
        common::{EntityType, Status},
    },
};
use ng_gateway_macros::IntoActiveValue;
use sea_orm::{entity::prelude::*, FromJsonQueryResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "pipeline")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub key: String,
    pub name: String,
    pub sampling: SamplingStrategy,
    pub roi_regions: RoiRegions,
    pub annotation: AnnotationConfig,
    pub status: Status,
    pub revision: u32,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Clone, Debug, PartialEq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult)]
pub struct RoiRegions(pub Vec<RegionOfInterest>);

/// Rectangular region of interest within a frame.
///
/// All coordinates are normalized to `[0.0, 1.0]` relative to frame dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RegionOfInterest {
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

impl RegionOfInterest {
    /// Full frame (no cropping).
    pub const FULL: Self = Self {
        x_min: 0.0,
        y_min: 0.0,
        x_max: 1.0,
        y_max: 1.0,
    };

    /// Validate that coordinates are within `[0.0, 1.0]` and min < max.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.x_min >= 0.0
            && self.y_min >= 0.0
            && self.x_max <= 1.0
            && self.y_max <= 1.0
            && self.x_min < self.x_max
            && self.y_min < self.y_max
    }
}

/// Annotation rendering configuration.
#[derive(Debug, Clone, PartialEq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult)]
pub struct AnnotationConfig {
    /// Master switch for annotation rendering.
    ///
    /// When `false`, no annotation work is performed regardless of other flags.
    /// Useful for saving CPU/memory when no live preview client is connected
    /// and alarm snapshots are not needed. Setting this to `"on_demand"` in
    /// future versions will auto-enable based on consumer presence (WebRTC
    /// preview, alarm triggers).
    #[serde(default = "AnnotationConfig::default_bool")]
    pub enabled: bool,
    /// Draw bounding boxes.
    #[serde(default = "AnnotationConfig::default_bool")]
    pub draw_bboxes: bool,
    /// Draw class labels on boxes.
    #[serde(default = "AnnotationConfig::default_bool")]
    pub draw_labels: bool,
    /// Draw confidence scores.
    #[serde(default = "AnnotationConfig::default_bool")]
    pub draw_confidence: bool,
    /// Draw tracking IDs.
    #[serde(default = "AnnotationConfig::default_bool")]
    pub draw_track_ids: bool,
    /// Draw segmentation overlays.
    #[serde(default = "AnnotationConfig::default_bool")]
    pub draw_segmentation: bool,
    /// Segmentation overlay alpha in range `[0.0, 1.0]`.
    #[serde(default = "AnnotationConfig::default_segmentation_alpha")]
    pub segmentation_alpha: f32,
    /// Background class index to ignore for segmentation overlay.
    ///
    /// Set `None` to render all classes.
    #[serde(default = "AnnotationConfig::default_segmentation_background_class")]
    pub segmentation_background_class: Option<u8>,
    /// Bounding box line thickness (pixels).
    #[serde(default = "AnnotationConfig::default_line_thickness")]
    pub line_thickness: u32,
    /// Font scale for labels.
    #[serde(default = "AnnotationConfig::default_font_scale")]
    pub font_scale: f32,
    /// JPEG output quality (1-100).
    #[serde(default = "AnnotationConfig::default_jpeg_quality")]
    pub jpeg_quality: u8,
    /// Output image max dimension (downscale if larger, for bandwidth).
    pub max_output_dimension: Option<u32>,
    /// Color palette for classes (hex colors, cycles if fewer than classes).
    #[serde(default = "AnnotationConfig::default_color_palette")]
    pub color_palette: Vec<String>,
    /// Behavior when annotation queue is full.
    #[serde(default)]
    pub queue_overflow_strategy: AnnotationQueueOverflowStrategy,
    /// Max enqueue wait time in milliseconds when strategy is `wait_for_slot`.
    #[serde(default = "AnnotationConfig::default_annotation_enqueue_timeout_ms")]
    pub enqueue_timeout_ms: u64,
}

impl Default for AnnotationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            draw_bboxes: true,
            draw_labels: true,
            draw_confidence: true,
            draw_track_ids: true,
            draw_segmentation: true,
            segmentation_alpha: 0.4,
            segmentation_background_class: Some(0),
            line_thickness: 2,
            font_scale: 0.6,
            jpeg_quality: 75,
            max_output_dimension: Some(1280),
            color_palette: AnnotationConfig::default_color_palette(),
            queue_overflow_strategy: AnnotationQueueOverflowStrategy::DropNewest,
            enqueue_timeout_ms: AnnotationConfig::default_annotation_enqueue_timeout_ms(),
        }
    }
}

impl AnnotationConfig {
    fn default_bool() -> bool {
        true
    }
    fn default_segmentation_alpha() -> f32 {
        0.4
    }
    fn default_segmentation_background_class() -> Option<u8> {
        Some(0)
    }
    fn default_color_palette() -> Vec<String> {
        vec![
            "#FF3838".into(),
            "#FF9D97".into(),
            "#FF701F".into(),
            "#FFB21D".into(),
            "#CFD231".into(),
            "#48F90A".into(),
            "#92CC17".into(),
            "#3DDB86".into(),
            "#1A9334".into(),
            "#00D4BB".into(),
            "#2C99A8".into(),
            "#00C2FF".into(),
            "#344593".into(),
            "#6473FF".into(),
            "#0018EC".into(),
            "#8438FF".into(),
            "#520085".into(),
            "#CB38FF".into(),
            "#FF95C8".into(),
            "#FF37C7".into(),
        ]
    }
    fn default_line_thickness() -> u32 {
        2
    }
    fn default_font_scale() -> f32 {
        0.6
    }
    fn default_jpeg_quality() -> u8 {
        75
    }
    fn default_annotation_enqueue_timeout_ms() -> u64 {
        5
    }
}

/// Preprocessing configuration — Inference stage internal, config-driven.
///
/// Users select resize mode and normalization preset in the Pipeline UI.
/// The engine maps these to the appropriate internal [`PreProcessor`] implementation.
#[derive(Debug, Clone, PartialEq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult)]
pub struct PreProcessorConfig {
    /// Resize strategy.
    pub resize_mode: Option<ResizeMode>,
    /// Normalization preset or custom values.
    pub normalization: Option<NormalizationConfig>,
    /// Channel order.
    pub channel_order: Option<ChannelOrder>,
    /// Letterbox padding fill value (0-255, default 114).
    pub pad_value: Option<u8>,
}

/// Postprocessing configuration — Inference stage internal, config-driven.
///
/// The engine auto-selects the postprocessor based on model task and output shape.
/// Users can override or fine-tune parameters here.
#[derive(Debug, Clone, PartialEq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult)]
pub struct PostProcessorConfig {
    /// Force a specific postprocessor type (overrides auto-detection).
    pub r#type: Option<PostProcessorType>,
    /// Top-K for classification models.
    pub top_k: Option<usize>,
    /// Whether to apply softmax (classification).
    pub apply_softmax: Option<bool>,
    /// Max detections after NMS (detection models).
    pub max_detections: Option<usize>,
    /// Number of keypoints per detection (pose models, default: 17 for COCO).
    pub num_keypoints: Option<usize>,
    /// Anomaly score threshold (anomaly detection models).
    pub anomaly_threshold: Option<f32>,
    /// NMS algorithm variant.
    pub nms_variant: Option<NmsVariantConfig>,
    /// Sigma parameter for Soft-NMS Gaussian decay (only when nms_variant = "soft").
    pub soft_nms_sigma: Option<f32>,
    /// Minimum prediction count to enable parallel candidate generation.
    pub detection_parallel_threshold: Option<usize>,
    /// Candidate pre-screen multiplier before NMS.
    pub nms_prescreen_multiplier: Option<usize>,
    /// Class-count threshold for classification small-input fast path.
    pub classification_small_class_fast_path: Option<usize>,
    /// Minimum pixel count to enable segmentation argmax parallelism.
    pub segmentation_parallel_min_pixels: Option<usize>,
}

/// Normalization configuration for preprocessing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizationConfig {
    /// Preset name.
    pub preset: Option<NormalizationPreset>,
    /// Custom mean `[R, G, B]` (only when preset = "custom").
    pub mean: Option<[f32; 3]>,
    /// Custom std `[R, G, B]` (only when preset = "custom").
    pub std: Option<[f32; 3]>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::pipeline_stage::Entity")]
    Stages,
    #[sea_orm(has_many = "super::alarm_rule::Entity")]
    AlarmRules,
    #[sea_orm(has_many = "super::pipeline_binding::Entity")]
    Bindings,
}

impl Related<super::pipeline_stage::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Stages.def()
    }
}

impl Related<super::alarm_rule::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AlarmRules.def()
    }
}

impl Related<super::pipeline_binding::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Bindings.def()
    }
}

impl NGEntity for Model {
    fn entity_type(&self) -> EntityType {
        EntityType::Pipeline
    }

    fn id(&self) -> Option<i32> {
        Some(self.id)
    }

    fn status(&self) -> Option<Status> {
        Some(self.status)
    }
}

impl NGEntity for ActiveModel {
    fn entity_type(&self) -> EntityType {
        EntityType::Pipeline
    }

    fn id(&self) -> Option<i32> {
        self.id.to_owned().take()
    }

    fn status(&self) -> Option<Status> {
        self.status.to_owned().take()
    }
}

impl ActiveModelBehavior for ActiveModel {}
