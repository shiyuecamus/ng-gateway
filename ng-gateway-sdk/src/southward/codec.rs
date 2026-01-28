use crate::{DataType, DriverError, DriverResult, NGValue, Transform};
use bytes::Bytes;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use std::sync::Arc;

/// Protocol-agnostic value coercion utilities.
/// Centralizes scaling, rounding, clamping, and common string/boolean parsing.
pub struct ValueCodec;

impl ValueCodec {
    /// The maximum integer magnitude that can be represented exactly in IEEE-754 `f64`.
    ///
    /// When downlink inverse transform is enabled (scale/offset/negate), the pipeline requires
    /// converting the logical numeric value into `f64`. For 64-bit integers beyond this limit,
    /// such conversion becomes lossy and would silently corrupt "never write wrong values".
    const F64_EXACT_INT_MAX_U64: u64 = 9_007_199_254_740_992; // 2^53

    #[inline]
    pub fn logical_to_wire_value(
        value: &NGValue,
        logical_dt: DataType,
        wire_dt: DataType,
        t: &Transform,
    ) -> DriverResult<NGValue> {
        if !value.validate_datatype(logical_dt) {
            return Err(DriverError::ValidationError(format!(
                "type mismatch: expected logical {:?}, got {:?}",
                logical_dt,
                value.data_type()
            )));
        }

        if !logical_dt.is_numeric() || !wire_dt.is_numeric() {
            return Self::non_numeric_logical_to_wire(value, logical_dt, wire_dt, t);
        }

        // Numeric identity mapping: allow strict datatype cast but never apply inverse.
        if t.is_identity_numeric() {
            return Self::numeric_identity_logical_to_wire(value, logical_dt, wire_dt);
        }

        // Numeric inverse mapping: requires a safe `f64` pipeline.
        let y = Self::numeric_value_to_f64_strict(value, logical_dt)?;
        let x = Self::inverse_transform_f64_strict(y, t)?;
        Self::box_f64_to_wire_strict(x, wire_dt)
    }

    /// Coerce a **wire-layer** `NGValue` into a **logical-layer** `NGValue` (uplink).
    ///
    /// # Design goal
    /// Keep driver code aligned with existing `coerce_*_to_value` APIs while enforcing a
    /// single, consistent uplink policy:
    /// - Decode protocol payload into a **wire** `NGValue` that matches `wire_dt`
    /// - Then call this function once to apply `Transform` and box into `logical_dt`
    ///
    /// # Safety policy (important)
    /// When a non-identity numeric transform is configured, this conversion requires an `f64`
    /// intermediate. For 64-bit integers beyond \(2^{53}\), `f64` cannot represent all integers
    /// exactly. To avoid silent corruption, we reject such cases.
    #[inline]
    pub fn wire_to_logical_value(
        value: &NGValue,
        wire_dt: DataType,
        logical_dt: DataType,
        t: &Transform,
    ) -> DriverResult<NGValue> {
        if !value.validate_datatype(wire_dt) {
            return Err(DriverError::ValidationError(format!(
                "type mismatch: expected wire {:?}, got {:?}",
                wire_dt,
                value.data_type()
            )));
        }

        // If logical is non-numeric, we only allow identity transform and strict (wire==logical).
        // For numeric logical types we support "numeric-like" wire encodings such as String/Binary.
        if !logical_dt.is_numeric() {
            if !t.is_identity_numeric() {
                return Err(DriverError::ConfigurationError(format!(
                    "non-numeric uplink cannot apply numeric transform: wire={:?}, logical={:?}, transform={:?}",
                    wire_dt, logical_dt, t
                )));
            }
            if wire_dt != logical_dt {
                return Err(DriverError::ValidationError(format!(
                    "non-numeric uplink does not support wire/logical datatype mapping: wire={:?}, logical={:?}",
                    wire_dt, logical_dt
                )));
            }
            return Ok(value.clone());
        }

        // Numeric fast path: keep integer precision when transform is identity.
        if t.is_identity_numeric() && wire_dt == logical_dt {
            return Ok(value.clone());
        }

        // Enforce 2^53 safety when a numeric transform is configured.
        // Note: `coerce_i64_to_value/coerce_u64_to_value` internally uses `as f64` when transform is enabled.
        if !t.is_identity_numeric() {
            match &value {
                NGValue::UInt64(v) => {
                    if *v > Self::F64_EXACT_INT_MAX_U64 {
                        return Err(DriverError::ValidationError(format!(
                            "numeric uplink value too large for safe transform (UInt64 > 2^53): {v}"
                        )));
                    }
                }
                NGValue::Int64(v) => {
                    if v.unsigned_abs() > Self::F64_EXACT_INT_MAX_U64 {
                        return Err(DriverError::ValidationError(format!(
                            "numeric uplink value too large for safe transform (Int64 magnitude > 2^53): {v}"
                        )));
                    }
                }
                _ => {}
            }
        }

        let out = match value {
            NGValue::Boolean(b) => Self::coerce_bool_to_value(*b, logical_dt, t),
            NGValue::Int8(v) => Self::coerce_i64_to_value(*v as i64, logical_dt, t),
            NGValue::UInt8(v) => Self::coerce_u64_to_value(*v as u64, logical_dt, t),
            NGValue::Int16(v) => Self::coerce_i64_to_value(*v as i64, logical_dt, t),
            NGValue::UInt16(v) => Self::coerce_u64_to_value(*v as u64, logical_dt, t),
            NGValue::Int32(v) => Self::coerce_i64_to_value(*v as i64, logical_dt, t),
            NGValue::UInt32(v) => Self::coerce_u64_to_value(*v as u64, logical_dt, t),
            NGValue::Int64(v) => Self::coerce_i64_to_value(*v, logical_dt, t),
            NGValue::UInt64(v) => Self::coerce_u64_to_value(*v, logical_dt, t),
            NGValue::Float32(v) => Self::coerce_f64_to_value(*v as f64, logical_dt, t),
            NGValue::Float64(v) => Self::coerce_f64_to_value(*v, logical_dt, t),
            // Timestamp is treated as epoch-ms numeric in the SDK model.
            NGValue::Timestamp(ms) => Self::coerce_i64_to_value(*ms, logical_dt, t),
            // Numeric-like wire encodings (compatibility):
            // - String: parse to f64 (supports hex "0x..." and decimals via SDK cast policy)
            // - Binary: interpret as BE f32/f64 via SDK cast policy
            //
            // Then reuse the unified numeric coerce pipeline so rounding/scale rules remain consistent.
            NGValue::String(_) | NGValue::Binary(_) => f64::try_from(value)
                .ok()
                .and_then(|n| Self::coerce_f64_to_value(n, logical_dt, t)),
        };

        out.ok_or(
            DriverError::ValidationError(format!(
                "uplink wire->logical coercion failed: wire={wire_dt:?}, logical={logical_dt:?}, value={value:?}, transform={t:?}"
            ))
        )
    }

    #[inline]
    fn non_numeric_logical_to_wire(
        value: &NGValue,
        logical_dt: DataType,
        wire_dt: DataType,
        t: &Transform,
    ) -> DriverResult<NGValue> {
        if !t.is_identity_numeric() {
            return Err(DriverError::ConfigurationError(format!(
                "non-numeric downlink cannot apply numeric transform: logical={:?}, wire={:?}, transform={:?}",
                logical_dt, wire_dt, t
            )));
        }
        if logical_dt != wire_dt {
            return Err(DriverError::ValidationError(format!(
                "non-numeric downlink does not support logical/wire datatype mapping: logical={:?}, wire={:?}",
                logical_dt, wire_dt
            )));
        }
        Ok(value.clone())
    }

    #[inline]
    fn numeric_identity_logical_to_wire(
        value: &NGValue,
        logical_dt: DataType,
        wire_dt: DataType,
    ) -> DriverResult<NGValue> {
        if logical_dt == wire_dt {
            return Ok(value.clone());
        }
        let cast_err = |e: crate::NGValueCastError| {
            DriverError::ValidationError(format!(
                "downlink datatype cast failed: logical={logical_dt:?} -> wire={wire_dt:?}, error={e}",
            ))
        };
        match wire_dt {
            DataType::Int8 => Ok(NGValue::Int8(i8::try_from(value).map_err(cast_err)?)),
            DataType::UInt8 => Ok(NGValue::UInt8(u8::try_from(value).map_err(cast_err)?)),
            DataType::Int16 => Ok(NGValue::Int16(i16::try_from(value).map_err(cast_err)?)),
            DataType::UInt16 => Ok(NGValue::UInt16(u16::try_from(value).map_err(cast_err)?)),
            DataType::Int32 => Ok(NGValue::Int32(i32::try_from(value).map_err(cast_err)?)),
            DataType::UInt32 => Ok(NGValue::UInt32(u32::try_from(value).map_err(cast_err)?)),
            DataType::Int64 => Ok(NGValue::Int64(i64::try_from(value).map_err(cast_err)?)),
            DataType::UInt64 => Ok(NGValue::UInt64(u64::try_from(value).map_err(cast_err)?)),
            DataType::Float32 => Ok(NGValue::Float32(f32::try_from(value).map_err(cast_err)?)),
            DataType::Float64 => Ok(NGValue::Float64(f64::try_from(value).map_err(cast_err)?)),
            _ => Err(DriverError::ConfigurationError(format!(
                "unsupported wire numeric data type: {wire_dt:?}",
            ))),
        }
    }

    #[inline]
    fn numeric_value_to_f64_strict(value: &NGValue, logical_dt: DataType) -> DriverResult<f64> {
        let y = match logical_dt {
            DataType::UInt64 => {
                let v = u64::try_from(value).map_err(|e| {
                    DriverError::ValidationError(format!(
                        "numeric downlink value conversion failed: logical={logical_dt:?}, actual={:?}, error={e}",
                        value.data_type()
                    ))
                })?;
                if v > Self::F64_EXACT_INT_MAX_U64 {
                    return Err(DriverError::ValidationError(format!(
                        "numeric downlink value too large for safe transform (UInt64 > 2^53): {v}"
                    )));
                }
                v as f64
            }
            DataType::Int64 => {
                let v = i64::try_from(value).map_err(|e| {
                    DriverError::ValidationError(format!(
                        "numeric downlink value conversion failed: logical={logical_dt:?}, actual={:?}, error={e}",
                        value.data_type()
                    ))
                })?;
                if v.unsigned_abs() > Self::F64_EXACT_INT_MAX_U64 {
                    return Err(DriverError::ValidationError(format!(
                        "numeric downlink value too large for safe transform (Int64 magnitude > 2^53): {v}"
                    )));
                }
                v as f64
            }
            _ => f64::try_from(value).map_err(|e| {
                DriverError::ValidationError(format!(
                    "numeric downlink value conversion failed: logical={:?}, actual={:?}, error={e}",
                    logical_dt,
                    value.data_type()
                ))
            })?,
        };
        if y.is_finite() {
            Ok(y)
        } else {
            Err(DriverError::ValidationError(
                "numeric downlink value must be finite".to_string(),
            ))
        }
    }

    #[inline]
    fn inverse_transform_f64_strict(y: f64, t: &Transform) -> DriverResult<f64> {
        // Needed: inverse uses division by `scale`. `scale == 0.0` would produce +/-inf or NaN.
        if matches!(t.transform_scale, Some(s) if s == 0.0) {
            return Err(DriverError::ConfigurationError(
                "transform_scale must not be 0.0 for downlink inverse transform".to_string(),
            ));
        }
        let x = t.inverse_f64(y);
        if x.is_finite() {
            Ok(x)
        } else {
            Err(DriverError::ValidationError(
                "numeric downlink inverse transform produced non-finite value".to_string(),
            ))
        }
    }

    #[inline]
    fn box_f64_to_wire_strict(x: f64, wire_dt: DataType) -> DriverResult<NGValue> {
        let out = match wire_dt {
            DataType::Boolean | DataType::String | DataType::Binary | DataType::Timestamp => {
                return Err(DriverError::ConfigurationError(format!(
                    "wire datatype must be numeric here, got {wire_dt:?}"
                )))
            }
            DataType::Int8 => {
                let r = x.round();
                (r >= i8::MIN as f64 && r <= i8::MAX as f64).then(|| NGValue::Int8(r as i8))
            }
            DataType::UInt8 => {
                let r = x.round();
                (r >= 0.0 && r <= u8::MAX as f64).then(|| NGValue::UInt8(r as u8))
            }
            DataType::Int16 => {
                let r = x.round();
                (r >= i16::MIN as f64 && r <= i16::MAX as f64).then(|| NGValue::Int16(r as i16))
            }
            DataType::UInt16 => {
                let r = x.round();
                (r >= 0.0 && r <= u16::MAX as f64).then(|| NGValue::UInt16(r as u16))
            }
            DataType::Int32 => {
                let r = x.round();
                (r >= i32::MIN as f64 && r <= i32::MAX as f64).then(|| NGValue::Int32(r as i32))
            }
            DataType::UInt32 => {
                let r = x.round();
                (r >= 0.0 && r <= u32::MAX as f64).then(|| NGValue::UInt32(r as u32))
            }
            DataType::Int64 => {
                let r = x.round();
                if (r < -(Self::F64_EXACT_INT_MAX_U64 as f64)
                    || r > (Self::F64_EXACT_INT_MAX_U64 as f64))
                    || (r < i64::MIN as f64 || r > i64::MAX as f64)
                {
                    None
                } else {
                    Some(NGValue::Int64(r as i64))
                }
            }
            DataType::UInt64 => {
                let r = x.round();
                if r < 0.0 || r > (Self::F64_EXACT_INT_MAX_U64 as f64) {
                    None
                } else {
                    Some(NGValue::UInt64(r as u64))
                }
            }
            DataType::Float32 => Some(NGValue::Float32(x as f32)),
            DataType::Float64 => Some(NGValue::Float64(x)),
        };

        out.ok_or(DriverError::ValidationError(format!(
            "downlink value out of range after inverse: wire={:?}, value={x}",
            wire_dt
        )))
    }

    #[inline]
    pub fn apply_transform_f64(x: f64, t: &Transform) -> f64 {
        t.apply_f64(x)
    }

    #[inline]
    fn should_apply_numeric_transform(expected: DataType, t: &Transform) -> bool {
        expected.is_numeric() && !t.is_identity_numeric()
    }

    /// Apply numeric transform only when it is meaningful for the expected type.
    ///
    /// This helper centralizes the "should we apply transform?" gate to avoid repeating
    /// the same branching in multiple coercion paths.
    #[inline]
    fn apply_transform_f64_if_needed(x: f64, expected: DataType, t: &Transform) -> f64 {
        if Self::should_apply_numeric_transform(expected, t) {
            Self::apply_transform_f64(x, t)
        } else {
            x
        }
    }

    /// Coerce a numeric value (already transformed if needed) into the expected `DataType`.
    ///
    /// Important: This function must **not** apply `Transform` again.
    #[inline]
    fn coerce_f64_to_value_after_transform(value: f64, expected: DataType) -> Option<NGValue> {
        match expected {
            DataType::Boolean => {
                if !value.is_finite() {
                    None
                } else {
                    Some(NGValue::Boolean(value != 0.0))
                }
            }
            DataType::Int8 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v >= i8::MIN as f64 && v <= i8::MAX as f64 {
                    Some(NGValue::Int8(v as i8))
                } else {
                    None
                }
            }
            DataType::UInt8 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v >= 0.0 && v <= u8::MAX as f64 {
                    Some(NGValue::UInt8(v as u8))
                } else {
                    None
                }
            }
            DataType::Int16 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v >= i16::MIN as f64 && v <= i16::MAX as f64 {
                    Some(NGValue::Int16(v as i16))
                } else {
                    None
                }
            }
            DataType::UInt16 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v >= 0.0 && v <= u16::MAX as f64 {
                    Some(NGValue::UInt16(v as u16))
                } else {
                    None
                }
            }
            DataType::Int32 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v >= i32::MIN as f64 && v <= i32::MAX as f64 {
                    Some(NGValue::Int32(v as i32))
                } else {
                    None
                }
            }
            DataType::UInt32 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v >= 0.0 && v <= u32::MAX as f64 {
                    Some(NGValue::UInt32(v as u32))
                } else {
                    None
                }
            }
            DataType::Int64 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v < i64::MIN as f64 || v > i64::MAX as f64 {
                    return None;
                }
                Some(NGValue::Int64(v as i64))
            }
            DataType::UInt64 => {
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v < 0.0 || v > u64::MAX as f64 {
                    return None;
                }
                Some(NGValue::UInt64(v as u64))
            }
            DataType::Float32 => Some(NGValue::Float32(value as f32)),
            DataType::Float64 => Some(NGValue::Float64(value)),
            DataType::String => Some(NGValue::String(Arc::<str>::from(value.to_string()))),
            DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(
                &value.to_be_bytes(),
            ))),
            DataType::Timestamp => {
                // Timestamp is represented as Unix epoch milliseconds in `i64`.
                // Avoid float-to-int wrap on extreme values.
                let v = value.round();
                if !v.is_finite() {
                    return None;
                }
                if v < i64::MIN as f64 || v > i64::MAX as f64 {
                    return None;
                }
                Some(NGValue::Timestamp(v as i64))
            }
        }
    }

    /// Apply optional numeric transform and return an `NGValue` in the expected `DataType`.
    ///
    /// # Notes
    /// - This is the **recommended** hot-path conversion API for drivers.
    /// - Timestamp/Binary require protocol-specific parsing and are not supported here.
    #[inline]
    pub fn coerce_bool_to_value(value: bool, expected: DataType, t: &Transform) -> Option<NGValue> {
        match expected {
            DataType::Boolean => Some(NGValue::Boolean(value)),
            DataType::String => Some(NGValue::String(Arc::<str>::from(if value {
                "true"
            } else {
                "false"
            }))),
            DataType::Binary => Some(NGValue::Binary(Bytes::from_static(if value {
                &[1u8; 1]
            } else {
                &[0u8; 1]
            }))),
            DataType::Timestamp => None,
            // All other types are numeric-like here; delegate to the unified numeric path.
            _ => Self::coerce_f64_to_value(if value { 1.0 } else { 0.0 }, expected, t),
        }
    }

    /// Coerce a numeric value (`f64`) into an expected `DataType` with optional transform.
    ///
    /// # Performance
    /// This avoids `serde_json::Value` allocations and should be used in hot paths.
    #[inline]
    pub fn coerce_f64_to_value(value: f64, expected: DataType, t: &Transform) -> Option<NGValue> {
        let value = Self::apply_transform_f64_if_needed(value, expected, t);
        Self::coerce_f64_to_value_after_transform(value, expected)
    }

    /// Coerce an unsigned integer source into an expected `DataType` with optional transform.
    ///
    /// # Performance & correctness
    /// - When `scale` is `None` and the target is an integer type, this avoids any `f64`
    ///   roundtrip and therefore preserves full integer precision.
    /// - When `scale` is `Some(_)`, we apply scaling in `f64` and then delegate to
    ///   `coerce_f64_to_value` for consistent rounding behavior.
    #[inline]
    pub fn coerce_u64_to_value(value: u64, expected: DataType, t: &Transform) -> Option<NGValue> {
        if Self::should_apply_numeric_transform(expected, t) {
            let v = Self::apply_transform_f64(value as f64, t);
            return Self::coerce_f64_to_value_after_transform(v, expected);
        }
        match expected {
            DataType::Boolean => Some(NGValue::Boolean(value != 0)),
            DataType::UInt8 => u8::try_from(value).ok().map(NGValue::UInt8),
            DataType::UInt16 => u16::try_from(value).ok().map(NGValue::UInt16),
            DataType::UInt32 => u32::try_from(value).ok().map(NGValue::UInt32),
            DataType::UInt64 => Some(NGValue::UInt64(value)),
            DataType::Int8 => i8::try_from(value).ok().map(NGValue::Int8),
            DataType::Int16 => i16::try_from(value).ok().map(NGValue::Int16),
            DataType::Int32 => i32::try_from(value).ok().map(NGValue::Int32),
            DataType::Int64 => i64::try_from(value).ok().map(NGValue::Int64),
            DataType::Float32 => Some(NGValue::Float32(value as f32)),
            DataType::Float64 => Some(NGValue::Float64(value as f64)),
            DataType::String => Some(NGValue::String(Arc::<str>::from(value.to_string()))),
            DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(
                &value.to_be_bytes(),
            ))),
            DataType::Timestamp => {
                // Avoid u64 -> i64 wrap.
                if value > i64::MAX as u64 {
                    None
                } else {
                    Some(NGValue::Timestamp(value as i64))
                }
            }
        }
    }

    /// Coerce a signed integer source into an expected `DataType` with optional transform.
    ///
    /// See `coerce_u64_to_value` for performance semantics.
    #[inline]
    pub fn coerce_i64_to_value(value: i64, expected: DataType, t: &Transform) -> Option<NGValue> {
        if Self::should_apply_numeric_transform(expected, t) {
            let v = Self::apply_transform_f64(value as f64, t);
            return Self::coerce_f64_to_value_after_transform(v, expected);
        }
        match expected {
            DataType::Boolean => Some(NGValue::Boolean(value != 0)),
            DataType::Int8 => i8::try_from(value).ok().map(NGValue::Int8),
            DataType::Int16 => i16::try_from(value).ok().map(NGValue::Int16),
            DataType::Int32 => i32::try_from(value).ok().map(NGValue::Int32),
            DataType::Int64 => Some(NGValue::Int64(value)),
            DataType::UInt8 => u8::try_from(value).ok().map(NGValue::UInt8),
            DataType::UInt16 => u16::try_from(value).ok().map(NGValue::UInt16),
            DataType::UInt32 => u32::try_from(value).ok().map(NGValue::UInt32),
            DataType::UInt64 => u64::try_from(value).ok().map(NGValue::UInt64),
            DataType::Float32 => Some(NGValue::Float32(value as f32)),
            DataType::Float64 => Some(NGValue::Float64(value as f64)),
            DataType::String => Some(NGValue::String(Arc::<str>::from(value.to_string()))),
            DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(
                &value.to_be_bytes(),
            ))),
            DataType::Timestamp => Some(NGValue::Timestamp(value)),
        }
    }

    /// Time helpers: centralize rendering to string or epoch-ms conversions for drivers.
    #[inline]
    pub fn time_of_day_to_ms(t: NaiveTime) -> u64 {
        (t.num_seconds_from_midnight() as u64) * 1000 + (t.nanosecond() / 1_000_000) as u64
    }

    #[inline]
    pub fn duration_to_ms(d: Duration) -> i64 {
        d.num_milliseconds()
    }

    #[inline]
    pub fn date_to_epoch_ms(d: NaiveDate) -> Option<i64> {
        let ndt = d.and_time(NaiveTime::from_hms_opt(0, 0, 0)?);
        let dt = DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc);
        Some(dt.timestamp_millis())
    }

    #[inline]
    pub fn datetime_to_epoch_ms(ndt: NaiveDateTime) -> i64 {
        let dt = DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc);
        dt.timestamp_millis()
    }

    /// Convert a byte slice into a lower-case hex string with "0x" prefix.
    #[inline]
    pub fn bytes_to_hex_string(bytes: &[u8]) -> String {
        const LUT: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(2 + bytes.len() * 2);
        out.push_str("0x");
        for &b in bytes {
            out.push(LUT[(b >> 4) as usize] as char);
            out.push(LUT[(b & 0x0F) as usize] as char);
        }
        out
    }

    /// Decode a hex string into bytes. Accepts optional "0x"/"0X" prefix and odd length (pads low nibble with 0).
    #[inline]
    pub fn hex_string_to_bytes(s: &str) -> Option<Vec<u8>> {
        let st = s.trim();
        let hex = if st.starts_with("0x") || st.starts_with("0X") {
            &st[2..]
        } else {
            st
        };
        if hex.is_empty() {
            return Some(Vec::new());
        }
        let bytes = hex.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len().div_ceil(2));
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16)? as u8;
            let lo = (bytes[i + 1] as char).to_digit(16)? as u8;
            out.push((hi << 4) | (lo & 0x0F));
            i += 2;
        }
        if i < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16)? as u8;
            out.push(hi << 4);
        }
        Some(out)
    }

    /// Parse a JSON value as epoch milliseconds (UTC).
    /// - String: RFC3339 parsed in any offset, converted to UTC ms
    /// - Number: treated as epoch milliseconds
    #[inline]
    pub fn json_to_timestamp_ms(v: &serde_json::Value) -> Option<i64> {
        if let Some(s) = v.as_str() {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s.trim()) {
                return Some(dt.timestamp_millis());
            }
        }
        if let Some(n) = v.as_i64() {
            return Some(n);
        }
        if let Some(n) = v.as_u64() {
            return Some(n as i64);
        }
        if let Some(n) = v.as_f64() {
            if !n.is_finite() {
                return None;
            }
            let r = n.round();
            if r < i64::MIN as f64 || r > i64::MAX as f64 {
                return None;
            }
            return Some(r as i64);
        }
        None
    }
}
