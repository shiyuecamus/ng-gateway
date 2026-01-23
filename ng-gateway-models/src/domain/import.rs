use ng_gateway_sdk::{FieldError, ValidatedRow};
use serde::Serialize;
use std::collections::HashMap;
use validator::Validate;

use crate::domain::prelude::{NewAction, NewDevice, NewPoint};

/// A grouping accumulator for `FlattenEntity::DevicePoints` imports.
///
/// # Purpose
/// The `DevicePoints` template represents a logical "device" repeated across multiple rows
/// (one per point). We group by `device_name` and validate that device-level fields are
/// consistent within the same group.
#[derive(Debug, Clone)]
pub struct DeviceGroup {
    /// The reference `device_type` extracted from the first row of this device group.
    pub ref_device_type: Option<String>,
    /// The reference `device_driver_config` extracted from the first row of this device group.
    pub ref_device_config: serde_json::Value,
    /// All rows belonging to this device group.
    pub rows: Vec<ValidatedRow>,
}

/// A per-device reference snapshot used for consistency checks.
#[derive(Debug, Clone)]
pub struct DeviceRef {
    /// The reference `device_type` extracted from the first seen row for a device.
    pub device_type: Option<String>,
    /// The reference `device_driver_config` extracted from the first seen row for a device.
    pub device_driver_config: serde_json::Value,
}

/// Prepared payload for channel `import-device-commit` (devices only).
///
/// This is an internal helper type to avoid defining ad-hoc structs inside handlers.
#[derive(Debug, Clone)]
pub struct PreparedDeviceCommit {
    /// Total number of rows read from the template.
    pub total_rows: usize,
    /// Warning count produced by validation/normalization.
    pub warn_count: usize,
    /// Number of rows that passed field-level validation.
    pub valid_count: usize,
    /// Devices mapped from validated rows.
    pub devices: Vec<NewDevice>,
    /// Row/field errors from validation.
    pub errors: Vec<FieldError>,
}

/// Prepared payload for channel `import-device-points-commit` (devices + points).
///
/// This is an internal helper type to avoid defining ad-hoc structs inside handlers.
#[derive(Debug, Clone)]
pub struct PreparedDevicePointsCommit {
    /// Total number of rows read from the template.
    pub total_rows: usize,
    /// Warning count produced by validation/normalization.
    pub warn_count: usize,
    /// Locale resolved from template metadata.
    pub locale: String,
    /// Devices mapped from validated rows (one per device group).
    pub devices: Vec<NewDevice>,
    /// Map: `device_name` -> `device_type` (used to match created device IDs).
    pub device_name_to_type: HashMap<String, String>,
    /// Map: `device_name` -> point rows (after removing device-level fields).
    pub points_by_device: HashMap<String, Vec<ValidatedRow>>,
    /// Row/field errors collected during parsing/validation/mapping.
    pub errors: Vec<FieldError>,
}

/// Prepared payload for device `import-point-commit`.
///
/// This is an internal helper type to avoid defining ad-hoc structs inside handlers.
#[derive(Debug, Clone)]
pub struct PreparedPointCommit {
    /// Total number of rows read from the template.
    pub total_rows: usize,
    /// Warning count produced by validation/normalization.
    pub warn_count: usize,
    /// Number of rows that passed field-level validation.
    pub valid_count: usize,
    /// Points mapped from validated rows.
    pub points: Vec<NewPoint>,
    /// Row/field errors from validation.
    pub errors: Vec<FieldError>,
}

/// Prepared payload for device `import-action-commit`.
///
/// This is an internal helper type to avoid defining ad-hoc structs inside handlers.
#[derive(Debug, Clone)]
pub struct PreparedActionCommit {
    /// Total number of rows read from the template.
    pub total_rows: usize,
    /// Warning count produced by validation/normalization.
    pub warn_count: usize,
    /// Number of rows that passed field-level validation.
    pub valid_count: usize,
    /// Actions mapped (and potentially aggregated) from validated rows.
    pub actions: Vec<NewAction>,
    /// Row/field errors from validation.
    pub errors: Vec<FieldError>,
}

/// Import preview response for Excel imports.
#[derive(Debug, Clone, Serialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    /// Total number of rows read from the template.
    pub total_rows: usize,
    /// Number of valid rows (no blocking errors).
    #[serde(default)]
    pub valid: usize,
    /// Number of invalid rows (with blocking errors).
    #[serde(default)]
    pub invalid: usize,
    /// Number of warnings encountered (non-blocking).
    #[serde(default)]
    pub warn: usize,
    /// A small subset of field errors for preview.
    #[serde(default)]
    pub errors: Vec<FieldError>,
}

/// Import commit result for Excel imports.
#[derive(Debug, Clone, Serialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    /// Total number of rows read from the template.
    pub total_rows: usize,
    /// Number of inserted entities (devices/points/actions/etc).
    pub inserted: usize,
    /// Number of valid rows (no blocking errors).
    #[serde(default)]
    pub valid: usize,
    /// Number of invalid rows (with blocking errors).
    #[serde(default)]
    pub invalid: usize,
    /// Number of warnings encountered (non-blocking).
    #[serde(default)]
    pub warn: usize,
    /// A small subset of field errors for preview.
    #[serde(default)]
    pub errors: Vec<FieldError>,
}
