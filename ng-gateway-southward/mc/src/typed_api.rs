use crate::{
    codec::McCodec,
    protocol::{
        error::{Error as McError, Result as McResult},
        frame::addr::McLogicalAddress,
        planner::{McReadItemRaw, PlannerConfig, PointReadSpec, WriteEntry},
        session::{MultiAddressReadSpec, MultiAddressWriteSpec, Session},
    },
};
use bytes::Bytes;
use ng_gateway_sdk::{DataType, NGValue};

/// High-level typed API facade for MC protocol built on top of `Session`.
///
/// This module is the single entry point that higher layers (drivers, actions)
/// should use when they need typed read/write semantics expressed in terms of
/// `DataType`. Internally it delegates to the lower-level raw `Session` helpers
/// and performs (de)serialization via `McCodec`. This keeps the `protocol`
/// module purely MC‑semantic and byte‑oriented.
pub struct McTypedApi;

/// Typed read result item for MC protocol.
///
/// This mirrors the previous `McReadItemTyped` representation but lives in the
/// typed facade so that the protocol layer no longer depends on JSON or
/// gateway `DataType` semantics.
#[derive(Debug, Clone)]
pub struct McReadItemTyped {
    /// Index into the original request list.
    pub index: usize,
    /// MC completion code for this item (0 means success).
    pub end_code: u16,
    /// Decoded typed value when `end_code == 0` and decode succeeded.
    ///
    /// # Note
    /// This is intentionally `NGValue` to avoid `serde_json::Value` on hot paths.
    pub value: Option<NGValue>,
}

/// Typed logical read specification for a single MC item.
///
/// This type is consumed by the typed facade and converted into raw
/// `PointReadSpec` instances for the planner/session layer.
#[derive(Debug, Clone)]
pub struct TypedPointReadSpec {
    /// Caller-supplied index used to keep a stable mapping between input order
    /// and merged read results.
    pub index: usize,
    /// Logical data type for this item, used for typed decoding.
    pub data_type: DataType,
    /// Parsed logical address.
    pub addr: McLogicalAddress,
    /// Number of logical words for this point.
    pub word_len: u16,
    /// Encoded device code used by MC protocol.
    pub device_code: u16,
}

/// Typed random-read specification for multi-address helpers.
#[derive(Debug, Clone)]
#[allow(unused)]
pub struct TypedMultiAddressReadSpec {
    /// Caller-supplied index to preserve input ordering in results.
    pub index: usize,
    /// Logical MC address to read from.
    pub addr: crate::protocol::frame::addr::McLogicalAddress,
    /// Logical data type expected for this address.
    pub data_type: DataType,
}

/// Typed random-write specification for multi-address helpers.
#[allow(unused)]
#[derive(Debug, Clone)]
pub struct TypedMultiAddressWriteSpec {
    /// Logical MC address to write to.
    pub addr: crate::protocol::frame::addr::McLogicalAddress,
    /// Logical data type of the value.
    pub data_type: DataType,
    /// JSON-encoded value to be written.
    pub value: serde_json::Value,
}

#[allow(unused)]
impl McTypedApi {
    /// Typed batch read wrapper over `Session::read_points_raw`.
    ///
    /// Callers should prepare `PointReadSpec` instances using the planner
    /// helpers and pass a `PlannerConfig` derived from channel configuration.
    pub async fn read_points_typed(
        session: &Session,
        planner_cfg: &PlannerConfig,
        specs: Vec<TypedPointReadSpec>,
    ) -> McResult<Vec<McReadItemTyped>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        // Build raw specs for protocol layer.
        let mut raw_specs: Vec<PointReadSpec> = Vec::with_capacity(specs.len());
        for s in specs.iter() {
            raw_specs.push(PointReadSpec {
                index: s.index,
                addr: s.addr.clone(),
                word_len: s.word_len,
                device_code: s.device_code,
            });
        }

        // Execute raw read.
        let raw_items: Vec<McReadItemRaw> = session.read_points_raw(planner_cfg, raw_specs).await?;

        // Map by index to recover typed results.
        let mut out = Vec::with_capacity(raw_items.len());
        for raw in raw_items.into_iter() {
            let data_type = specs
                .iter()
                .find(|s| s.index == raw.index)
                .map(|s| s.data_type)
                .unwrap_or(DataType::Binary);

            let value = match (raw.end_code, raw.payload.as_ref()) {
                (0, Some(payload)) => McCodec::decode(data_type, payload.as_ref()).ok(),
                _ => None,
            };

            out.push(McReadItemTyped {
                index: raw.index,
                end_code: raw.end_code,
                value,
            });
        }

        Ok(out)
    }

    /// Typed batch write wrapper over `Session::write_points_raw`.
    pub async fn write_points_typed(
        session: &Session,
        planner_cfg: &PlannerConfig,
        entries: Vec<WriteEntry>,
    ) -> McResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        session.write_points_raw(planner_cfg, entries).await
    }

    /// Typed multi-address read wrapper over `Session::read_multi_address_raw`.
    pub async fn read_multi_address_typed(
        session: &Session,
        specs: Vec<TypedMultiAddressReadSpec>,
    ) -> McResult<Vec<McReadItemTyped>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        // Build raw specs.
        let mut raw_specs: Vec<MultiAddressReadSpec> = Vec::with_capacity(specs.len());
        for spec in specs.iter() {
            raw_specs.push(MultiAddressReadSpec {
                index: spec.index,
                addr: spec.addr.clone(),
            });
        }

        let raw_items = session.read_multi_address_raw(raw_specs).await?;

        // Map index -> DataType for decode.
        let mut out = Vec::with_capacity(raw_items.len());
        for raw in raw_items.into_iter() {
            let data_type = specs
                .iter()
                .find(|s| s.index == raw.index)
                .map(|s| s.data_type)
                .unwrap_or(DataType::Binary);

            let value = match (raw.end_code, raw.payload.as_ref()) {
                (0, Some(payload)) => McCodec::decode(data_type, payload.as_ref()).ok(),
                _ => None,
            };

            out.push(McReadItemTyped {
                index: raw.index,
                end_code: raw.end_code,
                value,
            });
        }

        Ok(out)
    }

    /// Typed multi-address write wrapper over `Session::write_multi_address_raw`.
    pub async fn write_multi_address_typed(
        session: &Session,
        specs: Vec<TypedMultiAddressWriteSpec>,
    ) -> McResult<()> {
        if specs.is_empty() {
            return Ok(());
        }

        let mut raw_specs: Vec<MultiAddressWriteSpec> = Vec::with_capacity(specs.len());
        for spec in specs.into_iter() {
            let data =
                McCodec::encode(spec.data_type, &spec.value).map_err(|_| McError::Encode {
                    context: "failed to encode value for random write",
                })?;

            raw_specs.push(MultiAddressWriteSpec {
                addr: spec.addr,
                data: Bytes::from(data),
            });
        }

        session.write_multi_address_raw(raw_specs).await
    }
}
