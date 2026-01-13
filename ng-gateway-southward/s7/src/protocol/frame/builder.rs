use super::{
    comm::S7Header,
    cotp::Cotp,
    cotp_param::CotpCrParams,
    cotp_param::CotpDataParams,
    iter::S7VarSpec as VarSpec,
    owned::{
        S7AckDataPayloadOwned, S7JobParamOwned, S7JobPayloadOwned, S7ParamAckDataOwned,
        S7UserDataParamItemOwned, S7UserDataParamOwned, S7UserDataPayloadOwned,
        UserDataPayloadItemOwned,
    },
    types::{
        CpuFunctionGroup, CpuFunctionType, S7DataVariableType, S7PduType, S7ReturnCode,
        S7TransportSize,
    },
    S7AppBody, S7Message, S7Pdu, Tpkt, WireEncode,
};
use bytes::{BufMut, Bytes, BytesMut};

/// Build a full S7 payload (header + param + data) into an owned Bytes buffer.
/// This returns only the S7 portion (to be wrapped by codec as COTP(Data)+TPKT).
pub fn build_s7_payload(pdu_ref: u16, param_bytes: &[u8], data: Option<&[u8]>) -> Bytes {
    let param_len = param_bytes.len() as u16;
    let payload_len = data.map(|d| d.len() as u16).unwrap_or(0);

    let header = S7Header {
        protocol_id: 0x32,
        pdu_type: S7PduType::Job,
        reserved: 0,
        pdu_ref,
        param_len,
        payload_len,
        error_code: None,
    };

    let mut buf = BytesMut::with_capacity(10 + param_bytes.len() + payload_len as usize);
    header.encode_to(&mut buf);
    buf.put_slice(param_bytes);
    if let Some(d) = data {
        buf.put_slice(d);
    }
    buf.freeze()
}

// Map logical String/WString transport sizes to wire-level Byte with fixed maximum length
// to ensure valid S7Any transport size on the wire. Keep other specs unchanged.
#[inline]
fn map_spec_for_read(s: &VarSpec) -> VarSpec {
    match s.transport_size {
        S7TransportSize::Bit => *s,
        _ => VarSpec {
            transport_size: S7TransportSize::Byte,
            count: s
                .count
                .saturating_mul(s.transport_size.element_bytes() as u16),
            db_number: s.db_number,
            area: s.area,
            byte_address: s.byte_address,
            bit_index: 0,
        },
    }
}

// For write, also map String/WString to Byte and set count to actual data length in bytes,
// which is the encoded STRING/WSTRING struct length. Other types remain unchanged.
#[inline]
fn map_spec_for_write(s: &VarSpec, data_len: usize) -> VarSpec {
    match s.transport_size {
        S7TransportSize::Bit => *s,
        _ => VarSpec {
            transport_size: S7TransportSize::Byte,
            count: (data_len as u32).min(u16::MAX as u32) as u16,
            db_number: s.db_number,
            area: s.area,
            byte_address: s.byte_address,
            bit_index: 0,
        },
    }
}

/// Build a ReadVar Job payload from a list of variable specifications.
/// This encodes S7 header + ReadVar parameter and returns raw S7 bytes to be sent via codec.
pub fn build_read_var(pdu_ref: u16, specs: &[VarSpec]) -> Bytes {
    let owned_param = S7JobParamOwned::ReadVarRequest {
        specs: specs.iter().map(map_spec_for_read).collect(),
    };
    build_owned_job_pdu(pdu_ref, owned_param, None).into_bytes()
}

/// Build a WriteVar Job payload from (spec,data) tuples.
/// Data items are encoded according to S7 write semantics: first byte 0x00, a transport-kind byte,
/// a u16 length field (bits for Byte/Word/DWord, bytes for Bit), followed by the data bytes.
pub fn build_write_var(pdu_ref: u16, items: &[(VarSpec, &[u8])]) -> Bytes {
    let specs: Vec<VarSpec> = items
        .iter()
        .map(|(s, d)| map_spec_for_write(s, d.len()))
        .collect();
    let owned_param = S7JobParamOwned::WriteVarRequest { specs };
    let payload_items: Vec<(VarSpec, Bytes)> = items
        .iter()
        .map(|(s, d)| (*s, Bytes::copy_from_slice(d)))
        .collect();
    let owned_payload = S7JobPayloadOwned::WriteVarRequest {
        items: payload_items,
    };
    build_owned_job_pdu(pdu_ref, owned_param, Some(owned_payload)).into_bytes()
}

/// Build S7 SetupCommunication Job payload (S7-only bytes)
pub fn build_setup_comm(
    pdu_ref: u16,
    preferred_pdu_size: Option<u16>,
    preferred_amq_caller: Option<u16>,
    preferred_amq_callee: Option<u16>,
) -> Bytes {
    build_owned_job_pdu(
        pdu_ref,
        S7JobParamOwned::SetupCommunication {
            amq_caller: preferred_amq_caller.unwrap_or(8),
            amq_callee: preferred_amq_callee.unwrap_or(8),
            pdu_len: preferred_pdu_size.unwrap_or(480),
        },
        None,
    )
    .into_bytes()
}

/// Build `S7Message` from a COTP TPDU
pub fn build_message_from_cotp(cotp: Cotp) -> S7Message {
    S7Message {
        tpkt: Tpkt {
            version: 0x03,
            reserved: 0x00,
            length: 0,
        },
        cotp,
        app: None,
    }
}

/// Build COTP CR `S7Message`
pub fn build_cotp_cr_message(params: CotpCrParams) -> S7Message {
    build_message_from_cotp(Cotp::Cr(params))
}

/// Build COTP Data `S7Message` with default params
pub fn build_cotp_data_message(payload: Bytes) -> S7Message {
    let mut msg = build_message_from_cotp(Cotp::D(CotpDataParams::default()));
    msg.app = Some(S7AppBody::Segmented(payload));
    msg
}

/// Build COTP Data `S7Message` with explicit params
pub fn build_cotp_data_with_message(payload: Bytes, params: CotpDataParams) -> S7Message {
    let mut msg = build_message_from_cotp(Cotp::D(params));
    msg.app = Some(S7AppBody::Segmented(payload));
    msg
}

/// Build a single CPU Functions user-data item param and payload pair as a full UserData PDU
#[allow(clippy::too_many_arguments)]
pub fn build_userdata_cpu_functions_single(
    pdu_ref: u16,
    method: u8,
    cpu_function_type: CpuFunctionType,
    cpu_function_group: CpuFunctionGroup,
    cpu_subfunction: u8,
    sequence_number: u8,
    param_extra: Bytes,
    payload_return_code: S7ReturnCode,
    payload_transport_size: S7DataVariableType,
    payload_data: Bytes,
) -> S7Pdu {
    let param = S7UserDataParamOwned {
        item_count: 1,
        items: vec![S7UserDataParamItemOwned::CpuFunctions {
            method,
            cpu_function_type,
            cpu_function_group,
            cpu_subfunction,
            sequence_number,
            extra: param_extra,
        }],
    };

    let payload = S7UserDataPayloadOwned {
        item_count: 1,
        items: vec![UserDataPayloadItemOwned {
            return_code: payload_return_code,
            transport_size: payload_transport_size,
            data: payload_data,
        }],
    };

    build_owned_user_data_pdu(pdu_ref, param, payload)
}

/// Convenience builder: CPU Functions Read SZL Request (group=CpuFunctions, type=Request, sub=0x01)
/// param_extra should contain SZL request-specific parameters if any; empty for no-data variant.
pub fn build_userdata_read_szl_request(pdu_ref: u16, param_extra: Bytes) -> S7Pdu {
    build_userdata_cpu_functions_single(
        pdu_ref,
        0x11, // method commonly used by PLC4X for CPU Functions
        CpuFunctionType::Request,
        CpuFunctionGroup::CpuFunctions,
        0x01,
        0x00,
        param_extra,
        S7ReturnCode::Success,
        S7DataVariableType::ByteWordDWord,
        Bytes::new(),
    )
}

/// Build a complete owned S7 Job PDU from owned parameter and optional payload.
pub fn build_owned_job_pdu(
    pdu_ref: u16,
    param: S7JobParamOwned,
    payload: Option<S7JobPayloadOwned>,
) -> S7Pdu {
    let param_len = param.encoded_len(&()) as u16;
    let payload_len = payload
        .as_ref()
        .map(|p| p.encoded_len(&()) as u16)
        .unwrap_or(0);

    let header = S7Header {
        protocol_id: 0x32,
        pdu_type: S7PduType::Job,
        reserved: 0,
        pdu_ref,
        param_len,
        payload_len,
        error_code: None,
    };

    let mut pbuf = BytesMut::with_capacity(param_len as usize);
    let _ = param.encode_to(&mut pbuf, &());
    let param_bytes = pbuf.freeze();

    let payload_bytes = if let Some(p) = payload {
        let mut dbuf = BytesMut::with_capacity(payload_len as usize);
        let _ = p.encode_to(&mut dbuf, &());
        dbuf.freeze()
    } else {
        Bytes::new()
    };

    S7Pdu {
        header,
        param: param_bytes,
        payload: payload_bytes,
    }
}

/// Build a complete owned S7 AckData PDU
pub fn build_owned_ack_data_pdu(
    pdu_ref: u16,
    param: S7ParamAckDataOwned,
    payload: Option<S7AckDataPayloadOwned>,
) -> S7Pdu {
    let param_len = param.encoded_len(&()) as u16;
    let payload_len = payload
        .as_ref()
        .map(|p| p.encoded_len(&()) as u16)
        .unwrap_or(0);

    let header = S7Header {
        protocol_id: 0x32,
        pdu_type: S7PduType::AckData,
        reserved: 0,
        pdu_ref,
        param_len,
        payload_len,
        error_code: None,
    };

    let mut pbuf = BytesMut::with_capacity(param_len as usize);
    let _ = param.encode_to(&mut pbuf, &());
    let param_bytes = pbuf.freeze();

    let payload_bytes = if let Some(p) = payload {
        let mut dbuf = BytesMut::with_capacity(payload_len as usize);
        let _ = p.encode_to(&mut dbuf, &());
        dbuf.freeze()
    } else {
        Bytes::new()
    };

    S7Pdu {
        header,
        param: param_bytes,
        payload: payload_bytes,
    }
}

/// Build a complete owned S7 UserData PDU
pub fn build_owned_user_data_pdu(
    pdu_ref: u16,
    param: S7UserDataParamOwned,
    payload: S7UserDataPayloadOwned,
) -> S7Pdu {
    let param_len = param.encoded_len(&()) as u16;
    let payload_len = payload.encoded_len(&()) as u16;

    let header = S7Header {
        protocol_id: 0x32,
        pdu_type: S7PduType::UserData,
        reserved: 0,
        pdu_ref,
        param_len,
        payload_len,
        error_code: None,
    };

    let mut pbuf = BytesMut::with_capacity(param_len as usize);
    let _ = param.encode_to(&mut pbuf, &());
    let param_bytes = pbuf.freeze();

    let mut dbuf = BytesMut::with_capacity(payload_len as usize);
    let _ = payload.encode_to(&mut dbuf, &());
    let payload_bytes = dbuf.freeze();

    S7Pdu {
        header,
        param: param_bytes,
        payload: payload_bytes,
    }
}
