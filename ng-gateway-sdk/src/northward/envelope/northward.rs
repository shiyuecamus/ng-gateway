use crate::{
    envelope::{EnvelopeEvent, EnvelopeKind, EnvelopeMeta, WireEnvelope},
    northward::{NorthwardData, NorthwardEvent},
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Strongly-typed northward envelope that derives `event.kind` from `payload`.
///
/// # Why this exists
/// The wire envelope (`WireEnvelope<T>`) is a stable boundary format. It is intentionally
/// generic and stores the discriminator separately (`event.kind`) from the payload object
/// (`payload.data`).
///
/// This type provides an ergonomic, strongly-typed representation for gateway/plugin code:
/// - Serialization automatically writes `event.kind` from the payload's authoritative mapping.
/// - Deserialization uses `event.kind` to select the correct payload type and validates it.
/// - Unknown kinds are preserved as raw JSON for forward compatibility.
#[derive(Debug, Clone)]
pub struct NorthwardEnvelope {
    /// Schema version for evolution.
    pub schema_version: u32,
    /// Optional metadata envelope (uplink typically includes it; downlink may omit it).
    pub envelope: Option<EnvelopeMeta>,
    /// Strongly-typed payload.
    pub payload: NorthwardEnvelopePayload,
}

impl NorthwardEnvelope {
    /// Create a v1 envelope.
    #[inline]
    pub fn v1(payload: NorthwardEnvelopePayload) -> Self {
        Self {
            schema_version: 1,
            envelope: None,
            payload,
        }
    }

    /// Attach envelope metadata.
    #[inline]
    pub fn with_meta(mut self, meta: EnvelopeMeta) -> Self {
        self.envelope = Some(meta);
        self
    }

    /// Return the derived `EnvelopeKind` for routing.
    #[inline]
    pub fn kind(&self) -> EnvelopeKind {
        self.payload.envelope_kind()
    }

    /// Return the derived `EnvelopeEvent` for wire encoding.
    #[inline]
    pub fn event(&self) -> EnvelopeEvent {
        EnvelopeEvent { kind: self.kind() }
    }
}

/// Borrowed view of a northward envelope for **zero-clone serialization** on hot paths.
///
/// # Performance rationale
/// Uplink encoding frequently serializes large telemetry payloads. Cloning `NorthwardData`
/// (especially `Vec<PointValue>`) would be wasteful. This type borrows the payload while keeping
/// the exact same wire format as `NorthwardEnvelope`.
#[derive(Debug, Clone)]
pub struct NorthwardEnvelopeRef<'a> {
    /// Schema version for evolution.
    pub schema_version: u32,
    /// Optional metadata envelope (owned, small).
    pub envelope: Option<EnvelopeMeta>,
    /// Borrowed payload.
    pub payload: NorthwardEnvelopePayloadRef<'a>,
}

impl<'a> NorthwardEnvelopeRef<'a> {
    /// Create a v1 envelope.
    #[inline]
    pub fn v1(payload: NorthwardEnvelopePayloadRef<'a>) -> Self {
        Self {
            schema_version: 1,
            envelope: None,
            payload,
        }
    }

    /// Attach envelope metadata.
    #[inline]
    pub fn with_meta(mut self, meta: EnvelopeMeta) -> Self {
        self.envelope = Some(meta);
        self
    }

    /// Return the derived `EnvelopeKind` for routing.
    #[inline]
    pub fn kind(&self) -> EnvelopeKind {
        self.payload.envelope_kind()
    }

    /// Return the derived `EnvelopeEvent` for wire encoding.
    #[inline]
    pub fn event(&self) -> EnvelopeEvent {
        EnvelopeEvent { kind: self.kind() }
    }
}

/// Strongly-typed payload for `NorthwardEnvelope`.
#[derive(Debug, Clone)]
pub enum NorthwardEnvelopePayload {
    /// Uplink data payload (Gateway -> platform).
    Data(NorthwardData),
    /// Downlink/control-plane event payload (platform -> Gateway via plugin).
    Event(NorthwardEvent),
    /// Unknown payload for forward compatibility.
    ///
    /// We preserve both the declared kind and the raw JSON payload to allow:
    /// - debugging / observability
    /// - pass-through routing
    /// - rolling upgrades (new producers with old consumers)
    Unknown { kind: EnvelopeKind, raw: Value },
}

/// Borrowed payload variants for `NorthwardEnvelopeRef`.
#[derive(Debug, Clone)]
pub enum NorthwardEnvelopePayloadRef<'a> {
    /// Uplink data payload (Gateway -> platform).
    Data(&'a NorthwardData),
    /// Downlink/control-plane event payload (platform -> Gateway via plugin).
    Event(&'a NorthwardEvent),
    /// Unknown payload for forward compatibility.
    Unknown { kind: EnvelopeKind, raw: &'a Value },
}

impl NorthwardEnvelopePayloadRef<'_> {
    /// Return the authoritative discriminator for this payload.
    #[inline]
    pub fn envelope_kind(&self) -> EnvelopeKind {
        match self {
            NorthwardEnvelopePayloadRef::Data(d) => d.envelope_kind(),
            NorthwardEnvelopePayloadRef::Event(e) => e.envelope_kind(),
            NorthwardEnvelopePayloadRef::Unknown { kind, .. } => *kind,
        }
    }
}

impl NorthwardEnvelopePayload {
    /// Return the authoritative discriminator for this payload.
    #[inline]
    pub fn envelope_kind(&self) -> EnvelopeKind {
        match self {
            NorthwardEnvelopePayload::Data(d) => d.envelope_kind(),
            NorthwardEnvelopePayload::Event(e) => e.envelope_kind(),
            NorthwardEnvelopePayload::Unknown { kind, .. } => *kind,
        }
    }
}

impl From<NorthwardData> for NorthwardEnvelopePayload {
    #[inline]
    fn from(value: NorthwardData) -> Self {
        Self::Data(value)
    }
}

impl From<NorthwardEvent> for NorthwardEnvelopePayload {
    #[inline]
    fn from(value: NorthwardEvent) -> Self {
        Self::Event(value)
    }
}

/// Errors produced when decoding a `NorthwardEnvelope` from a `WireEnvelope<Value>`.
#[derive(Debug, Error)]
pub enum NorthwardEnvelopeDecodeError {
    /// Failed to decode payload data for the declared kind.
    #[error("payload decode failed (kind={kind:?}): {source}")]
    PayloadDecode {
        /// Declared envelope kind.
        kind: EnvelopeKind,
        /// JSON decode error.
        #[source]
        source: serde_json::Error,
    },
}

/// Serialize `NorthwardData` payload as the inner object **without enum tagging**.
///
/// This is required because the discriminator already lives in `event.kind`.
struct NorthwardDataNoTag<'a>(&'a NorthwardData);

impl Serialize for NorthwardDataNoTag<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            NorthwardData::DeviceConnected(d) => d.serialize(serializer),
            NorthwardData::DeviceDisconnected(d) => d.serialize(serializer),
            NorthwardData::Telemetry(t) => t.serialize(serializer),
            NorthwardData::Attributes(a) => a.serialize(serializer),
            NorthwardData::Alarm(a) => a.serialize(serializer),
            NorthwardData::RpcResponse(r) => r.serialize(serializer),
            NorthwardData::WritePointResponse(r) => r.serialize(serializer),
        }
    }
}

/// Serialize `NorthwardEvent` payload as the inner object **without enum tagging**.
///
/// This is required because the discriminator already lives in `event.kind`.
struct NorthwardEventNoTag<'a>(&'a NorthwardEvent);

impl Serialize for NorthwardEventNoTag<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            NorthwardEvent::RpcResponseReceived(r) => r.serialize(serializer),
            NorthwardEvent::CommandReceived(c) => c.serialize(serializer),
            NorthwardEvent::WritePoint(w) => w.serialize(serializer),
        }
    }
}

struct NorthwardPayloadNoTag<'a>(&'a NorthwardEnvelopePayload);

impl Serialize for NorthwardPayloadNoTag<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            NorthwardEnvelopePayload::Data(d) => NorthwardDataNoTag(d).serialize(serializer),
            NorthwardEnvelopePayload::Event(e) => NorthwardEventNoTag(e).serialize(serializer),
            NorthwardEnvelopePayload::Unknown { raw, .. } => raw.serialize(serializer),
        }
    }
}

#[derive(Serialize)]
struct WirePayloadView<'a> {
    data: NorthwardPayloadNoTag<'a>,
}

#[derive(Serialize)]
struct WireEnvelopeView<'a> {
    schema_version: u32,
    event: EnvelopeEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    envelope: &'a Option<EnvelopeMeta>,
    payload: WirePayloadView<'a>,
}

impl Serialize for NorthwardEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let wire = WireEnvelopeView {
            schema_version: self.schema_version,
            event: self.event(),
            envelope: &self.envelope,
            payload: WirePayloadView {
                data: NorthwardPayloadNoTag(&self.payload),
            },
        };
        wire.serialize(serializer)
    }
}

struct NorthwardPayloadRefNoTag<'a>(&'a NorthwardEnvelopePayloadRef<'a>);

impl Serialize for NorthwardPayloadRefNoTag<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            NorthwardEnvelopePayloadRef::Data(d) => NorthwardDataNoTag(d).serialize(serializer),
            NorthwardEnvelopePayloadRef::Event(e) => NorthwardEventNoTag(e).serialize(serializer),
            NorthwardEnvelopePayloadRef::Unknown { raw, .. } => raw.serialize(serializer),
        }
    }
}

#[derive(Serialize)]
struct WirePayloadRefView<'a> {
    data: NorthwardPayloadRefNoTag<'a>,
}

#[derive(Serialize)]
struct WireEnvelopeRefView<'a> {
    schema_version: u32,
    event: EnvelopeEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    envelope: &'a Option<EnvelopeMeta>,
    payload: WirePayloadRefView<'a>,
}

impl Serialize for NorthwardEnvelopeRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let wire = WireEnvelopeRefView {
            schema_version: self.schema_version,
            event: self.event(),
            envelope: &self.envelope,
            payload: WirePayloadRefView {
                data: NorthwardPayloadRefNoTag(&self.payload),
            },
        };
        wire.serialize(serializer)
    }
}

fn decode_payload_by_kind(
    kind: EnvelopeKind,
    raw: Value,
) -> Result<NorthwardEnvelopePayload, NorthwardEnvelopeDecodeError> {
    // NOTE: Types are re-exported at crate root, but referencing them through northward::model
    // keeps the mapping centralized and explicit.
    use crate::northward::model::{
        AlarmData, AttributeData, ClientRpcResponse, Command, DeviceConnectedData,
        DeviceDisconnectedData, ServerRpcResponse, TelemetryData, WritePoint, WritePointResponse,
    };

    match kind {
        // ===== uplink kinds =====
        EnvelopeKind::DeviceConnected => {
            let d: DeviceConnectedData = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Data(
                NorthwardData::DeviceConnected(d),
            ))
        }
        EnvelopeKind::DeviceDisconnected => {
            let d: DeviceDisconnectedData = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Data(
                NorthwardData::DeviceDisconnected(d),
            ))
        }
        EnvelopeKind::Telemetry => {
            let t: TelemetryData = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Data(NorthwardData::Telemetry(t)))
        }
        EnvelopeKind::Attributes => {
            let a: AttributeData = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Data(NorthwardData::Attributes(a)))
        }
        EnvelopeKind::Alarm => {
            let a: AlarmData = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Data(NorthwardData::Alarm(a)))
        }
        EnvelopeKind::RpcResponse => {
            let r: ClientRpcResponse = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Data(NorthwardData::RpcResponse(
                r,
            )))
        }
        EnvelopeKind::WritePointResponse => {
            let r: WritePointResponse = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Data(
                NorthwardData::WritePointResponse(r),
            ))
        }

        // ===== downlink kinds =====
        EnvelopeKind::WritePoint => {
            let w: WritePoint = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Event(NorthwardEvent::WritePoint(
                w,
            )))
        }
        EnvelopeKind::CommandReceived => {
            let c: Command = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Event(
                NorthwardEvent::CommandReceived(c),
            ))
        }
        EnvelopeKind::RpcResponseReceived => {
            let r: ServerRpcResponse = serde_json::from_value(raw)
                .map_err(|source| NorthwardEnvelopeDecodeError::PayloadDecode { kind, source })?;
            Ok(NorthwardEnvelopePayload::Event(
                NorthwardEvent::RpcResponseReceived(r),
            ))
        }
    }
}

impl TryFrom<WireEnvelope<Value>> for NorthwardEnvelope {
    type Error = NorthwardEnvelopeDecodeError;

    fn try_from(value: WireEnvelope<Value>) -> Result<Self, Self::Error> {
        let payload = decode_payload_by_kind(value.event.kind, value.payload.data)?;
        Ok(Self {
            schema_version: value.schema_version,
            envelope: value.envelope,
            payload,
        })
    }
}

impl<'de> Deserialize<'de> for NorthwardEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire: WireEnvelope<Value> = WireEnvelope::deserialize(deserializer)?;
        NorthwardEnvelope::try_from(wire).map_err(serde::de::Error::custom)
    }
}
