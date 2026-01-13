use super::super::error::{Error, Result};
use super::{
    iter::S7VarSpec,
    r#ref::{ItemsCountParam, SetupParam},
    types::{
        CpuFunctionGroup, CpuFunctionType, S7DataVariableType, S7Function, S7ReturnCode,
        S7SyntaxId, S7TransportSize,
    },
    WireEncode,
};
use bytes::{BufMut, Bytes};

/// Top-level owned parameter wrapper for outbound PDUs.
///
/// This enum provides a single entry point for constructing parameters for all
/// supported PDU kinds (Job/AckData/UserData) with owned data. It delegates
/// concrete encoding to existing specialized owned types.
#[derive(Debug, Clone)]
pub enum S7ParamOwned {
    /// Parameters for Job PDUs
    Job(S7JobParamOwned),
    /// Parameters for AckData PDUs
    AckData(S7ParamAckDataOwned),
    /// Parameters for UserData PDUs
    UserData(S7UserDataParamOwned),
    /// Parameters for Ack PDUs (pass-through raw bytes if present)
    Ack(Bytes),
}

impl WireEncode for S7ParamOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match self {
            S7ParamOwned::Job(inner) => inner.encoded_len(ctx),
            S7ParamOwned::AckData(inner) => inner.encoded_len(ctx),
            S7ParamOwned::UserData(inner) => inner.encoded_len(ctx),
            S7ParamOwned::Ack(raw) => raw.len(),
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<()> {
        match self {
            S7ParamOwned::Job(inner) => inner.encode_to(dst, ctx),
            S7ParamOwned::AckData(inner) => inner.encode_to(dst, ctx),
            S7ParamOwned::UserData(inner) => inner.encode_to(dst, ctx),
            S7ParamOwned::Ack(raw) => {
                dst.put_slice(raw);
                Ok(())
            }
        }
    }
}

/// Top-level owned payload wrapper for outbound PDUs.
///
/// This enum mirrors the three PDU kinds that carry payloads and provides a
/// unified encoding interface. `Empty` can be used for PDUs without payloads.
#[derive(Debug, Clone)]
pub enum S7PayloadOwned {
    /// Payload for Job PDUs
    Job(S7JobPayloadOwned),
    /// Payload for AckData PDUs
    AckData(S7AckDataPayloadOwned),
    /// Payload for UserData PDUs
    UserData(S7UserDataPayloadOwned),
    /// No payload
    Empty,
}

impl WireEncode for S7PayloadOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match self {
            S7PayloadOwned::Job(inner) => inner.encoded_len(ctx),
            S7PayloadOwned::AckData(inner) => inner.encoded_len(ctx),
            S7PayloadOwned::UserData(inner) => inner.encoded_len(ctx),
            S7PayloadOwned::Empty => 0,
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<()> {
        match self {
            S7PayloadOwned::Job(inner) => inner.encode_to(dst, ctx),
            S7PayloadOwned::AckData(inner) => inner.encode_to(dst, ctx),
            S7PayloadOwned::UserData(inner) => inner.encode_to(dst, ctx),
            S7PayloadOwned::Empty => Ok(()),
        }
    }
}

/// Owned S7 Job parameter variants for outbound encoding.
///
/// This mirrors a subset of `S7JobParamRef` but in owned form for building requests.
#[derive(Debug, Clone)]
pub enum S7JobParamOwned {
    /// Setup Communication job parameter (function 0xF0)
    SetupCommunication {
        amq_caller: u16,
        amq_callee: u16,
        pdu_len: u16,
    },
    /// ReadVar request with a list of variable specifications
    ReadVarRequest { specs: Vec<S7VarSpec> },
    /// WriteVar request with a list of variable specifications
    WriteVarRequest { specs: Vec<S7VarSpec> },
}

impl WireEncode for S7JobParamOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        match self {
            S7JobParamOwned::SetupCommunication { .. } => 1 + 1 + 2 + 2 + 2,
            S7JobParamOwned::ReadVarRequest { specs } => 1 + 1 + specs.len() * 12,
            S7JobParamOwned::WriteVarRequest { specs } => 1 + 1 + specs.len() * 12,
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        match self {
            S7JobParamOwned::SetupCommunication {
                amq_caller,
                amq_callee,
                pdu_len,
            } => {
                dst.put_u8(S7Function::SetupCommunication as u8);
                dst.put_u8(0x00);
                dst.put_u16(*amq_caller);
                dst.put_u16(*amq_callee);
                dst.put_u16(*pdu_len);
            }
            S7JobParamOwned::ReadVarRequest { specs } => {
                dst.put_u8(S7Function::ReadVar as u8);
                dst.put_u8(specs.len() as u8);
                for s in specs {
                    encode_var_spec(dst, s);
                }
            }
            S7JobParamOwned::WriteVarRequest { specs } => {
                dst.put_u8(S7Function::WriteVar as u8);
                dst.put_u8(specs.len() as u8);
                for s in specs {
                    encode_var_spec(dst, s);
                }
            }
        }
        Ok(())
    }
}

/// Owned S7 Job payload variants for outbound encoding.
#[derive(Debug, Clone)]
pub enum S7JobPayloadOwned {
    /// WriteVar request data items: (spec, data)
    WriteVarRequest { items: Vec<(S7VarSpec, Bytes)> },
}

impl WireEncode for S7JobPayloadOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        match self {
            S7JobPayloadOwned::WriteVarRequest { items } => items
                .iter()
                .map(|(_, data)| {
                    let pad = if (data.len() & 1) == 1 { 1 } else { 0 };
                    1 + 1 + 2 + data.len() + pad
                })
                .sum(),
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        match self {
            S7JobPayloadOwned::WriteVarRequest { items } => {
                for (spec, data) in items {
                    dst.put_u8(0x00);
                    let (dv_type, len_field): (S7DataVariableType, u16) = match spec.transport_size
                    {
                        S7TransportSize::Bit => (S7DataVariableType::Bit, data.len() as u16),
                        _ => (
                            S7DataVariableType::ByteWordDWord,
                            (data.len() as u32 * 8) as u16,
                        ),
                    };
                    dst.put_u8(dv_type as u8);
                    dst.put_u16(len_field);
                    dst.put_slice(data);
                    if (data.len() & 1) == 1 {
                        dst.put_u8(0x00);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Owned AckData parameter variants for outbound encoding (mainly for testing or mirroring).
#[derive(Debug, Clone)]
pub enum S7ParamAckDataOwned {
    SetupCommunication(SetupParam),
    ReadVarResponse(ItemsCountParam),
    WriteVarResponse(ItemsCountParam),
    Unknown { function: u8, raw: Bytes },
}

impl WireEncode for S7ParamAckDataOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        match self {
            S7ParamAckDataOwned::SetupCommunication(_) => 1 + 1 + 2 + 2 + 2,
            S7ParamAckDataOwned::ReadVarResponse(_) => 1 + 1,
            S7ParamAckDataOwned::WriteVarResponse(_) => 1 + 1,
            S7ParamAckDataOwned::Unknown { raw, .. } => 1 + raw.len(),
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        match self {
            S7ParamAckDataOwned::SetupCommunication(SetupParam {
                amq_caller,
                amq_callee,
                pdu_len,
                ..
            }) => {
                dst.put_u8(S7Function::SetupCommunication as u8);
                dst.put_u8(0x00);
                dst.put_u16(*amq_caller);
                dst.put_u16(*amq_callee);
                dst.put_u16(*pdu_len);
            }
            S7ParamAckDataOwned::ReadVarResponse(ItemsCountParam { item_count }) => {
                dst.put_u8(S7Function::ReadVar as u8);
                dst.put_u8(*item_count);
            }
            S7ParamAckDataOwned::WriteVarResponse(ItemsCountParam { item_count }) => {
                dst.put_u8(S7Function::WriteVar as u8);
                dst.put_u8(*item_count);
            }
            S7ParamAckDataOwned::Unknown { function, raw } => {
                dst.put_u8(*function);
                dst.put_slice(raw);
            }
        }
        Ok(())
    }
}

/// Owned UserData parameter (sequence of items) for outbound encoding.
#[derive(Debug, Clone)]
pub struct S7UserDataParamOwned {
    pub item_count: u8,
    pub items: Vec<S7UserDataParamItemOwned>,
}

#[derive(Debug, Clone)]
pub enum S7UserDataParamItemOwned {
    /// CPU Functions item type (0x12). We include minimal required fields and allow extra payload.
    CpuFunctions {
        method: u8,
        cpu_function_type: CpuFunctionType,
        cpu_function_group: CpuFunctionGroup,
        cpu_subfunction: u8,
        sequence_number: u8,
        extra: Bytes,
    },
    /// Unknown item type passthrough
    Unknown { item_type: u8, payload: Bytes },
}

impl WireEncode for S7UserDataParamOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        // sum of itemType(1)+length(1)+payload
        self.items
            .iter()
            .map(|it| match it {
                S7UserDataParamItemOwned::CpuFunctions { extra, .. } => 2 + (4 + extra.len()),
                S7UserDataParamItemOwned::Unknown { payload, .. } => 2 + payload.len(),
            })
            .sum()
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        for it in &self.items {
            match it {
                S7UserDataParamItemOwned::CpuFunctions {
                    method,
                    cpu_function_type,
                    cpu_function_group,
                    cpu_subfunction,
                    sequence_number,
                    extra,
                } => {
                    // itemType 0x12
                    dst.put_u8(0x12);
                    let payload_len = 4 + extra.len();
                    dst.put_u8(payload_len as u8);
                    dst.put_u8(*method);
                    let tg =
                        ((*cpu_function_type as u8) << 4) | ((*cpu_function_group as u8) & 0x0F);
                    dst.put_u8(tg);
                    dst.put_u8(*cpu_subfunction);
                    dst.put_u8(*sequence_number);
                    if !extra.is_empty() {
                        dst.put_slice(extra);
                    }
                }
                S7UserDataParamItemOwned::Unknown { item_type, payload } => {
                    dst.put_u8(*item_type);
                    dst.put_u8(payload.len() as u8);
                    dst.put_slice(payload);
                }
            }
        }
        Ok(())
    }
}

/// Owned UserData payload items
#[derive(Debug, Clone)]
pub struct S7UserDataPayloadOwned {
    pub item_count: u8,
    pub items: Vec<UserDataPayloadItemOwned>,
}

#[derive(Debug, Clone)]
pub struct UserDataPayloadItemOwned {
    pub return_code: S7ReturnCode,
    pub transport_size: S7DataVariableType,
    pub data: Bytes,
}

impl WireEncode for S7UserDataPayloadOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        self.items.iter().map(|it| 1 + 1 + 2 + it.data.len()).sum()
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        for it in &self.items {
            dst.put_u8(return_code_to_u8(&it.return_code));
            dst.put_u8(it.transport_size as u8);
            let len_field = encode_len_field(it.transport_size, it.data.len());
            dst.put_u16(len_field);
            dst.put_slice(&it.data);
        }
        Ok(())
    }
}

/// Owned AckData payload variants (structured helpers)
#[derive(Debug, Clone)]
pub enum S7AckDataPayloadOwned {
    ReadVarResponse {
        items: Vec<UserDataPayloadItemOwned>,
    },
    WriteVarResponse {
        items: Vec<S7ReturnCode>,
    },
    Unknown(Bytes),
}

impl WireEncode for S7AckDataPayloadOwned {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        match self {
            S7AckDataPayloadOwned::ReadVarResponse { items } => {
                items.iter().map(|it| 1 + 1 + 2 + it.data.len()).sum()
            }
            S7AckDataPayloadOwned::WriteVarResponse { items } => items.len(),
            S7AckDataPayloadOwned::Unknown(b) => b.len(),
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        match self {
            S7AckDataPayloadOwned::ReadVarResponse { items } => {
                for it in items {
                    dst.put_u8(return_code_to_u8(&it.return_code));
                    dst.put_u8(it.transport_size as u8);
                    let len_field = encode_len_field(it.transport_size, it.data.len());
                    dst.put_u16(len_field);
                    dst.put_slice(&it.data);
                }
            }
            S7AckDataPayloadOwned::WriteVarResponse { items } => {
                for rc in items {
                    dst.put_u8(return_code_to_u8(rc));
                }
            }
            S7AckDataPayloadOwned::Unknown(b) => dst.put_slice(b),
        }
        Ok(())
    }
}

fn encode_len_field(typ: S7DataVariableType, data_len_bytes: usize) -> u16 {
    match typ {
        S7DataVariableType::ByteWordDWord | S7DataVariableType::Integer => {
            (data_len_bytes as u32 * 8) as u16
        }
        _ => data_len_bytes as u16,
    }
}

fn return_code_to_u8(rc: &S7ReturnCode) -> u8 {
    match rc {
        S7ReturnCode::Reserved => 0x00,
        S7ReturnCode::Success => 0xFF,
        S7ReturnCode::HardwareFault => 0x01,
        S7ReturnCode::AccessDenied => 0x03,
        S7ReturnCode::AddressOutOfRange => 0x05,
        S7ReturnCode::DataTypeNotSupported => 0x06,
        S7ReturnCode::DataTypeInconsistent => 0x07,
        S7ReturnCode::ObjectDoesNotExist => 0x0A,
        S7ReturnCode::ObjectNotAvailable => 0x0B,
        S7ReturnCode::Unknown(v) => *v,
    }
}

fn encode_var_spec<B: BufMut>(dst: &mut B, s: &S7VarSpec) {
    dst.put_u8(0x12);
    dst.put_u8(0x0A);
    dst.put_u8(S7SyntaxId::S7Any as u8);
    dst.put_u8(s.transport_size as u8);
    dst.put_u16(s.count);
    dst.put_u16(s.db_number);
    dst.put_u8(s.area as u8);
    // Address field layout (24 bits total):
    // [ 5 bits reserved (0) | 16 bits byte address | 3 bits bit index ]
    // This must be written as exactly 3 bytes in big-endian order.
    let addr: u32 = ((s.byte_address & 0xFFFF) << 3) | ((s.bit_index as u32) & 0x07);
    dst.put_u8(((addr >> 16) & 0xFF) as u8);
    dst.put_u8(((addr >> 8) & 0xFF) as u8);
    dst.put_u8((addr & 0xFF) as u8);
}
