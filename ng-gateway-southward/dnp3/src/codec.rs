use crate::types::PointMeta;
use bytes::Bytes;
use ng_gateway_sdk::{DataType, NGValue, ValueCodec};
use std::sync::Arc;

/// DNP3 protocol specific codec helpers.
///
/// This module centralizes conversions between DNP3 typed values and
/// gateway-facing JSON values with optional scaling. Keeping this logic
/// here avoids duplication in the DNP3 read handler and keeps a clean
/// separation between:
/// - **protocol decode** (DNP3 library callbacks)
/// - **type/scale normalization** (this codec)
/// - **publishing** (northward)
pub struct Dnp3Codec;

impl Dnp3Codec {
    /// Convert a DNP3 boolean source into a typed `NGValue` according to `PointMeta`.
    #[inline]
    pub fn bool_to_value(value: bool, meta: &PointMeta) -> Option<NGValue> {
        ValueCodec::coerce_bool_to_value(value, meta.data_type, meta.scale)
    }

    /// Convert a DNP3 floating-point source into a typed `NGValue` according to `PointMeta`.
    #[inline]
    pub fn f64_to_value(value: f64, meta: &PointMeta) -> Option<NGValue> {
        ValueCodec::coerce_f64_to_value(value, meta.data_type, meta.scale)
    }

    /// Convert a DNP3 unsigned integer source into a typed `NGValue` according to `PointMeta`.
    #[inline]
    pub fn u64_to_value(value: u64, meta: &PointMeta) -> Option<NGValue> {
        ValueCodec::coerce_u64_to_value(value, meta.data_type, meta.scale)
    }

    /// Convert an Octet String (byte slice) into a typed `NGValue` according to `PointMeta`.
    ///
    /// Encoding rules:
    /// - `DataType::Binary`: `NGValue::Binary(Bytes)`
    /// - `DataType::String`: try UTF-8 decoding; if invalid UTF-8, fallback to `Binary`
    #[inline]
    pub fn octets_to_value(bytes: &[u8], meta: &PointMeta) -> Option<NGValue> {
        match meta.data_type {
            DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(bytes))),
            DataType::String => match std::str::from_utf8(bytes) {
                Ok(s) => Some(NGValue::String(Arc::<str>::from(s))),
                Err(_) => Some(NGValue::Binary(Bytes::copy_from_slice(bytes))),
            },
            _ => None,
        }
    }
}
