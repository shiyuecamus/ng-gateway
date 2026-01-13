use super::{
    super::types::McFrameVariant,
    route::{McRoute1E, McRoute3E4E},
};
use serde::{Deserialize, Serialize};

/// Common header fields shared by all MC request headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeaderReqCommon {
    /// Monitoring timer in 250 ms units.
    pub monitoring_timer: u16,
}

/// 1E frame request header (binary mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeader1EReq {
    /// Common header fields.
    pub common: McHeaderReqCommon,
    /// Sub-header (command code) for 1E frames (1 byte on the wire).
    pub sub_header: u8,
    /// Access route (PC number).
    pub route: McRoute1E,
}

impl McHeader1EReq {
    /// Compute the encoded byte length for this header.
    pub const fn byte_len() -> usize {
        // sub_header(1) + route(1) + monitoring_timer(2)
        1 + McRoute1E::byte_len() + 2
    }
}

/// 3E frame request header (binary mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeader3EReq {
    /// Common header fields.
    pub common: McHeaderReqCommon,
    /// Sub-header (2 bytes).
    pub sub_header: u16,
    /// Access route.
    pub route: McRoute3E4E,
    /// Data length in bytes from monitoring timer to the end of the PDU.
    pub data_length: u16,
}

impl McHeader3EReq {
    /// Compute the encoded byte length for this header.
    pub const fn byte_len() -> usize {
        // sub_header(2) + route(5) + data_length(2) + monitoring_timer(2)
        2 + McRoute3E4E::byte_len() + 2 + 2
    }
}

/// 4E frame request header (binary mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeader4EReq {
    /// Common header fields.
    pub common: McHeaderReqCommon,
    /// Sub-header (2 bytes).
    pub sub_header: u16,
    /// Serial number (2 bytes).
    pub serial_number: u16,
    /// Fixed number (2 bytes).
    pub fixed_number: u16,
    /// Access route.
    pub route: McRoute3E4E,
    /// Data length in bytes from monitoring timer to the end of the PDU.
    pub data_length: u16,
}

impl McHeader4EReq {
    /// Compute the encoded byte length for this header.
    pub const fn byte_len() -> usize {
        // sub_header(2) + serial(2) + fixed(2) + route(5) + data_length(2) + monitoring_timer(2)
        2 + 2 + 2 + McRoute3E4E::byte_len() + 2 + 2
    }
}

/// Common header fields for MC acknowledge headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeaderAckCommon {
    /// Frame variant (1E/3E/4E, binary/ASCII).
    pub frame_variant: McFrameVariant,
}

/// 1E acknowledge header (binary mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeader1EAck {
    /// Common ack fields.
    pub common: McHeaderAckCommon,
    /// Sub-header (1 byte).
    pub sub_header: u8,
    /// End code (1 byte).
    pub end_code: u8,
}

/// 3E acknowledge header (binary mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeader3EAck {
    /// Common ack fields.
    pub common: McHeaderAckCommon,
    /// Sub-header (2 bytes).
    pub sub_header: u16,
    /// Access route.
    pub route: McRoute3E4E,
    /// Data length.
    pub data_length: u16,
    /// End code.
    pub end_code: u16,
}

/// 4E acknowledge header (binary mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McHeader4EAck {
    /// Common ack fields.
    pub common: McHeaderAckCommon,
    /// Sub-header (2 bytes).
    pub sub_header: u16,
    /// Serial number (2 bytes).
    pub serial_number: u16,
    /// Fixed number (2 bytes).
    pub fixed_number: u16,
    /// Access route.
    pub route: McRoute3E4E,
    /// Data length.
    pub data_length: u16,
    /// End code.
    pub end_code: u16,
}

/// Unified MC header enum that covers both request and acknowledge headers.
///
/// This mirrors the `S7Header` + `S7Message` design on the S7 side and is used
/// by the codec/session layers to represent both directions of MC traffic in
/// a single strongly-typed container.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum McHeader {
    /// 1E request header.
    Req1E(McHeader1EReq),
    /// 3E request header.
    Req3E(McHeader3EReq),
    /// 4E request header.
    Req4E(McHeader4EReq),
    /// 1E acknowledge header.
    Ack1E(McHeader1EAck),
    /// 3E acknowledge header.
    Ack3E(McHeader3EAck),
    /// 4E acknowledge header.
    Ack4E(McHeader4EAck),
}
