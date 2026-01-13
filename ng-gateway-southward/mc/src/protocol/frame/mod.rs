pub mod addr;
pub mod builder;
pub mod command;
pub mod device;
pub mod header;
pub mod pdu;
pub mod route;

use self::{header::McHeader, pdu::McPdu};
use bytes::Bytes;

/// Application-layer MC body carried by a single MC frame.
///
/// This enum is purposely aligned with the S7 `S7AppBody` design: either
/// a raw segmented payload (for unknown/unparsed or partial data) or a fully
/// parsed `McPdu` instance.
#[derive(Debug, Clone)]
pub enum McAppBody {
    /// Transport-level raw segment (zero-copy). This data is not yet validated
    /// to be a full MC PDU and may represent a fragment.
    Raw(Bytes),
    /// Fully parsed MC PDU (semantic view).
    Parsed(McPdu),
}

/// A fully decoded MC wire packet (header + optional application body).
///
/// This is the MC counterpart of `S7Message` and is the item produced and
/// consumed by `McCodec`. Request and acknowledge frames are both represented
/// through the `McHeader` enum.
#[derive(Debug, Clone)]
pub struct McMessage {
    /// Request or acknowledge header.
    pub header: McHeader,
    /// Optional application body when the frame carries a PDU.
    pub body: Option<McAppBody>,
}
