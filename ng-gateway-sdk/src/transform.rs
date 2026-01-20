use crate::DataType;
use serde::{Deserialize, Serialize};

/// Logical-layer value transformation rules.
///
/// This type decouples **wire semantics** (how a protocol encodes values on the bus)
/// from **logical semantics** (how the gateway outputs `NGValue` and how API/UI validates).
///
/// # Design goals
/// - **Hot-path friendly**: small POD-like struct, `Copy`, no allocations.
/// - **Explicit**: all fields are optional except `negate`.
/// - **Composable**: drivers decide whether and when to apply it (uplink/downlink).
///
/// # Mathematical definition
/// Given a scalar numeric input \(x\), the forward transform produces \(y\):
/// \[
/// y = x \cdot s + o
/// \]
/// If `negate = true`, then \(y = -y\) after the affine transform.
///
/// The inverse transform for downlink is:
/// \[
/// x = (y - o) / s
/// \]
/// with the same `negate` applied first on \(y\).
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    /// Optional logical data type. When `None`, logical type follows wire type.
    pub transform_data_type: Option<DataType>,
    /// Optional scale factor \(s\). When `None`, defaults to \(1.0\).
    pub transform_scale: Option<f64>,
    /// Optional offset \(o\). When `None`, defaults to \(0.0\).
    pub transform_offset: Option<f64>,
    /// Whether to negate after affine transform.
    #[serde(default)]
    pub transform_negate: bool,
}

impl Transform {
    /// Returns true if this transform is an identity mapping for numeric values.
    ///
    /// Note: `datatype` does not affect numeric mapping itself; it affects boxing
    /// into `NGValue` and validation. Therefore `datatype` is ignored here.
    #[inline]
    pub fn is_identity_numeric(&self) -> bool {
        !self.transform_negate && self.transform_scale.is_none() && self.transform_offset.is_none()
    }

    /// Resolve the logical data type given a wire data type.
    #[inline]
    pub fn resolve_logical_datatype(&self, wire_dt: DataType) -> DataType {
        self.transform_data_type.unwrap_or(wire_dt)
    }

    /// Apply the forward numeric transform \(y = x*s + o\), then optional negate.
    #[inline]
    pub fn apply_f64(&self, x: f64) -> f64 {
        let mut y = x;
        if let Some(s) = self.transform_scale {
            y *= s;
        }
        if let Some(o) = self.transform_offset {
            y += o;
        }
        if self.transform_negate {
            y = -y;
        }
        y
    }

    /// Apply the inverse numeric transform for downlink.
    #[inline]
    pub fn inverse_f64(&self, y: f64) -> f64 {
        let mut x = y;
        if self.transform_negate {
            x = -x;
        }
        if let Some(o) = self.transform_offset {
            x -= o;
        }
        let s = self.transform_scale.unwrap_or(1.0);
        x / s
    }
}
