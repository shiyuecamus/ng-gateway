use super::model::{AttributeData, TelemetryData};
use crate::{DataPointType, NorthwardData, PointValue};
use chrono::Utc;
use std::collections::HashMap;

/// Per-business-device output buffers for building northward payloads.
///
/// # Purpose
/// Southward drivers typically batch-read points across multiple business devices, then
/// **fan out** decoded values into per-device `NorthwardData` objects (telemetry/attributes).
/// This helper provides a small, allocation-aware buffer type to avoid repeating that glue
/// logic across drivers while keeping hot-path overhead minimal.
///
/// # Notes
/// - The buffer does **not** perform any transformation/typing; drivers should push
///   already-decoded `PointValue` objects.
/// - Metadata fields are initialized as empty collections to match existing driver behavior.
#[derive(Debug, Default)]
pub struct DeviceBuffers {
    device_name: String,
    telemetry: Vec<PointValue>,
    attributes: Vec<PointValue>,
}

impl DeviceBuffers {
    /// Create buffers for a specific business device name.
    #[inline]
    pub fn new(device_name: impl Into<String>) -> Self {
        Self {
            device_name: device_name.into(),
            telemetry: Vec::new(),
            attributes: Vec::new(),
        }
    }

    /// Push a decoded point value into the correct bucket by `DataPointType`.
    #[inline]
    pub fn push(&mut self, kind: DataPointType, value: PointValue) {
        match kind {
            DataPointType::Telemetry => self.telemetry.push(value),
            DataPointType::Attribute => self.attributes.push(value),
        }
    }

    /// Return true if both telemetry and attributes are empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.telemetry.is_empty() && self.attributes.is_empty()
    }

    /// Convert these buffers into northward payload(s) with a stable timestamp.
    ///
    /// This will produce:
    /// - `NorthwardData::Telemetry` if telemetry values exist
    /// - `NorthwardData::Attributes` if attribute values exist
    ///
    /// If both exist, the device name is cloned once to satisfy ownership.
    #[inline]
    pub fn into_northward(
        self,
        device_id: i32,
        timestamp: chrono::DateTime<Utc>,
    ) -> Vec<NorthwardData> {
        let DeviceBuffers {
            device_name,
            telemetry,
            attributes,
        } = self;

        let has_telemetry = !telemetry.is_empty();
        let has_attributes = !attributes.is_empty();

        if !has_telemetry && !has_attributes {
            return Vec::new();
        }

        let mut out = Vec::with_capacity((has_telemetry as usize) + (has_attributes as usize));

        match (has_telemetry, has_attributes) {
            (true, true) => {
                out.push(NorthwardData::Telemetry(TelemetryData {
                    device_id,
                    device_name: device_name.clone(),
                    timestamp,
                    values: telemetry,
                    metadata: HashMap::new(),
                }));
                out.push(NorthwardData::Attributes(AttributeData {
                    device_id,
                    device_name,
                    timestamp,
                    client_attributes: attributes,
                    shared_attributes: Vec::new(),
                    server_attributes: Vec::new(),
                }));
            }
            (true, false) => {
                out.push(NorthwardData::Telemetry(TelemetryData {
                    device_id,
                    device_name,
                    timestamp,
                    values: telemetry,
                    metadata: HashMap::new(),
                }));
            }
            (false, true) => {
                out.push(NorthwardData::Attributes(AttributeData {
                    device_id,
                    device_name,
                    timestamp,
                    client_attributes: attributes,
                    shared_attributes: Vec::new(),
                    server_attributes: Vec::new(),
                }));
            }
            (false, false) => {}
        }

        out
    }
}
