use super::{
    command::McCommandKind,
    header::{McHeader, McHeader3EReq, McHeaderReqCommon},
    pdu::{McBody, McPdu, McRequestBody},
    route::McRoute3E4E,
    McAppBody, McMessage,
};
use crate::protocol::types::{McFrameVariant, McSeries};
use bytes::Bytes;

/// Build a 3E request header for a given frame variant and monitoring timeout.
///
/// The monitoring timer is encoded in units of 250 ms as per MC specification
/// and clamped to the valid `u16` range.
pub fn build_3e_req_header(
    frame_variant: McFrameVariant,
    monitoring_timeout_ms: u32,
) -> McHeader3EReq {
    let timer_units = (monitoring_timeout_ms / 250).max(1).min(u16::MAX as u32) as u16;
    McHeader3EReq {
        common: McHeaderReqCommon {
            monitoring_timer: timer_units,
        },
        sub_header: frame_variant.sub_header(),
        route: McRoute3E4E::default(),
        data_length: 0,
    }
}

/// Build a 3E batch word/dword read `McMessage` from logical parameters.
///
/// This helper is the MC counterpart of the S7 `build_read_var` function and
/// encapsulates the construction of `McPdu` and application header fields.
pub fn build_device_batch_read_message(
    series: McSeries,
    frame_variant: McFrameVariant,
    monitoring_timeout_ms: u32,
    head: u32,
    points: u16,
    device_code: u16,
) -> McMessage {
    let pdu = McPdu::new(
        series,
        McCommandKind::DeviceAccessBatchReadUnits,
        McBody::Request(McRequestBody::DeviceAccessBatchReadUnits {
            head,
            points,
            device_code,
        }),
    );
    let header = build_3e_req_header(frame_variant, monitoring_timeout_ms);
    McMessage {
        header: McHeader::Req3E(header),
        body: Some(McAppBody::Parsed(pdu)),
    }
}

/// Build a 3E batch word/dword write `McMessage` from logical parameters.
pub fn build_device_batch_write_message(
    series: McSeries,
    frame_variant: McFrameVariant,
    monitoring_timeout_ms: u32,
    head: u32,
    points: u16,
    device_code: u16,
    data: Bytes,
) -> McMessage {
    let pdu = McPdu::new(
        series,
        McCommandKind::DeviceAccessBatchWriteUnits,
        McBody::Request(McRequestBody::DeviceAccessBatchWriteUnits {
            head,
            points,
            device_code,
            data,
        }),
    );
    let header = build_3e_req_header(frame_variant, monitoring_timeout_ms);
    McMessage {
        header: McHeader::Req3E(header),
        body: Some(McAppBody::Parsed(pdu)),
    }
}

/// Build a 3E random read `McMessage` from logical word/dword address lists.
pub fn build_device_random_read_message(
    series: McSeries,
    frame_variant: McFrameVariant,
    monitoring_timeout_ms: u32,
    word_addrs: &[(u32, u16)],
    dword_addrs: &[(u32, u16)],
) -> McMessage {
    let pdu = McPdu::new(
        series,
        McCommandKind::DeviceAccessRandomReadUnits,
        McBody::Request(McRequestBody::DeviceAccessRandomReadUnits {
            word_addrs: word_addrs.to_vec(),
            dword_addrs: dword_addrs.to_vec(),
        }),
    );
    let header = build_3e_req_header(frame_variant, monitoring_timeout_ms);
    McMessage {
        header: McHeader::Req3E(header),
        body: Some(McAppBody::Parsed(pdu)),
    }
}

/// Build a 3E random write `McMessage` from logical word/dword item lists.
pub fn build_device_random_write_message(
    series: McSeries,
    frame_variant: McFrameVariant,
    monitoring_timeout_ms: u32,
    word_items: &[(u32, u16, Bytes)],
    dword_items: &[(u32, u16, Bytes)],
) -> McMessage {
    let word_vec: Vec<(u32, u16, Bytes)> = word_items
        .iter()
        .map(|(head, code, data)| (*head, *code, data.clone()))
        .collect();
    let dword_vec: Vec<(u32, u16, Bytes)> = dword_items
        .iter()
        .map(|(head, code, data)| (*head, *code, data.clone()))
        .collect();

    let pdu = McPdu::new(
        series,
        McCommandKind::DeviceAccessRandomWriteUnits,
        McBody::Request(McRequestBody::DeviceAccessRandomWriteUnits {
            word_items: word_vec,
            dword_items: dword_vec,
        }),
    );
    let header = build_3e_req_header(frame_variant, monitoring_timeout_ms);
    McMessage {
        header: McHeader::Req3E(header),
        body: Some(McAppBody::Parsed(pdu)),
    }
}
