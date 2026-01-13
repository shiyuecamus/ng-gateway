pub mod addr;
mod builder;
pub mod comm;
pub mod cotp;
pub mod cotp_param;
pub mod iter;
mod owned;
mod pdu;
mod r#ref;
pub mod tpkt;
pub mod tsap;
pub mod types;

use bytes::{BufMut, Bytes};
pub use ng_gateway_sdk::{WireDecode, WireEncode};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::str::FromStr;

/// Application-layer body carried by a COTP Data TPDU.
///
/// This enum is designed to be mutually exclusive to represent either a transport-level
/// segmented payload or a fully parsed S7 PDU. It helps enforce the invariant that a
/// message cannot simultaneously be both a partial transport segment and a complete S7 PDU.
#[derive(Debug, Clone)]
pub enum S7AppBody {
    /// Transport-level data segment (zero-copy). This data is not yet validated to be a full
    /// S7 PDU and may represent a fragment that requires reassembly by the session layer.
    Segmented(bytes::Bytes),
    /// Fully parsed S7 PDU (zero-copy slices for parameter and payload). This variant indicates
    /// that the underlying COTP Data TPDU carried a complete S7 PDU and has been successfully
    /// parsed by the codec layer.
    Parsed(S7Pdu),
}

/// A fully decoded wire packet including TPKT, COTP and an optional application body.
#[derive(Debug, Clone)]
pub struct S7Message {
    /// TPKT header extracted from the frame
    pub tpkt: Tpkt,
    /// Full COTP TPDU
    pub cotp: Cotp,
    /// Application body when the COTP TPDU is Data. None for non-Data TPDUs.
    pub app: Option<S7AppBody>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum CpuType {
    S7200 = 0,
    S7200Smart = 1,
    S7300 = 2,
    S7400 = 3,
    S71200 = 4,
    S71500 = 5,
    Logo0BA8 = 6,
}

pub use addr::parse_s7_address;
pub use builder::{
    build_cotp_cr_message, build_cotp_data_message, build_cotp_data_with_message,
    build_message_from_cotp, build_owned_ack_data_pdu, build_owned_job_pdu,
    build_owned_user_data_pdu, build_read_var, build_s7_payload, build_setup_comm,
    build_userdata_cpu_functions_single, build_userdata_read_szl_request, build_write_var,
};
pub use comm::S7Header;
pub use cotp::Cotp;
pub use cotp_param::{
    CotpCcParams, CotpCrParams, CotpDataParams, CotpDcParams, CotpDrParams, CotpReParams,
};
pub use iter::{
    S7UserDataParam, S7VarSpec, UserDataParamIter, UserDataPayloadItemIter, UserDataPayloadItemRef,
    VarPayloadDataItemIter, VarPayloadDataItemRef, VarPayloadStatusItemIter,
    VarPayloadStatusItemRef, VarSpecIter,
};
pub use owned::{
    S7AckDataPayloadOwned, S7JobParamOwned, S7JobPayloadOwned, S7ParamAckDataOwned, S7ParamOwned,
    S7PayloadOwned, S7UserDataParamOwned, S7UserDataPayloadOwned,
};
pub use pdu::{S7Pdu, S7PduRef};
pub use r#ref::{
    parse_param_ref, parse_payload_ref, CpuServiceParam, ItemsCountParam, ItemsParamRef,
    ReadVarResponsePayloadRef, S7AckDataPayloadRef, S7JobParamRef, S7JobPayloadRef,
    S7ParamAckDataRef, S7ParamRef, S7PayloadRef, S7UserDataParamRef, S7UserDataPayloadRef,
    SetupParam, UserDataPayloadRef, WriteVarRequestPayloadRef, WriteVarResponsePayloadRef,
};
pub use tpkt::Tpkt;
pub use tsap::{default_tsap_pair, Tsap, TsapPair};
pub(crate) use types::{
    decode_datetime8, decode_dtl12, latin1_bytes_to_string, s5time_to_duration,
    s7_date_to_naive_date, s7_tod_to_naive_time,
};
pub use types::{
    CotpType, S7Area, S7DataValue, S7Function, S7PduType, S7ReturnCode, S7SyntaxId, S7TransportSize,
};
