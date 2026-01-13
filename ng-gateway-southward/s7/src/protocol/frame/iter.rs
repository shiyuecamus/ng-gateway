use super::{
    super::error::{Error, Result},
    addr::S7Address,
    types::{
        CpuFunctionGroup, CpuFunctionType, S7Area, S7DataVariableType, S7ReturnCode, S7SyntaxId,
        S7TransportSize,
    },
};
use nom::number::complete::{be_u16, be_u32, u8 as nom_u8};

/// S7 Variable specification (S7ANY)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct S7VarSpec {
    pub transport_size: S7TransportSize,
    /// number of elements
    pub count: u16,
    /// DB number if area is DB, else 0
    pub db_number: u16,
    pub area: S7Area,
    /// byte offset
    pub byte_address: u32,
    /// bit index [0..7] for bit-level access
    pub bit_index: u8,
}

impl TryFrom<&S7Address> for S7VarSpec {
    type Error = Error;
    fn try_from(address: &S7Address) -> Result<Self> {
        Ok(Self {
            transport_size: address
                .transport_size
                .try_into()
                .map_err(|_| Error::ErrInvalidAddress(format!("{:?}", address)))?,
            count: 1,
            db_number: address.db_number,
            area: address
                .area
                .try_into()
                .map_err(|_| Error::ErrInvalidAddress(format!("{:?}", address)))?,
            byte_address: address.byte_address,
            bit_index: address.bit_index,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VarSpecIter<'a> {
    remaining: &'a [u8],
    left: u8,
}

impl<'a> VarSpecIter<'a> {
    pub fn new(item_count: u8, raw: &'a [u8]) -> Self {
        Self {
            remaining: raw,
            left: item_count,
        }
    }
}

impl<'a> Iterator for VarSpecIter<'a> {
    type Item = (S7VarSpec, &'a [u8]); // spec and new remaining slice
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        match parse_single_varspec(self.remaining) {
            Ok((rest, spec)) => {
                self.remaining = rest;
                self.left -= 1;
                Some((spec, rest))
            }
            Err(_) => None,
        }
    }
}

/// S7 User Data parameter (forward-compatible)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7UserDataParam {
    /// CPU Functions itemType = 0x12
    CpuFunctions {
        cpu_function_group: CpuFunctionGroup,
        cpu_function_type: CpuFunctionType,
        cpu_subfunction: u8,
    },
    /// Unknown item type; we only keep minimal info to remain `Copy`
    Unknown { item_type: u8, length: u8 },
}

#[derive(Debug, Clone, Copy)]
pub struct UserDataParamIter<'a> {
    remaining: &'a [u8],
    left: u8,
}

impl<'a> UserDataParamIter<'a> {
    pub fn new(item_count: u8, raw: &'a [u8]) -> Self {
        Self {
            remaining: raw,
            left: item_count,
        }
    }
}

impl<'a> Iterator for UserDataParamIter<'a> {
    type Item = (S7UserDataParam, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        match parse_single_userdata_item(self.remaining) {
            Ok((rest, item)) => {
                self.remaining = rest;
                self.left -= 1;
                Some((item, rest))
            }
            Err(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarPayloadDataItemRef<'a> {
    pub return_code: S7ReturnCode,
    pub transport_size: S7DataVariableType,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub struct VarPayloadDataItemIter<'a> {
    remaining: &'a [u8],
    left: u8,
}

impl<'a> VarPayloadDataItemIter<'a> {
    pub fn new(item_count: u8, raw: &'a [u8]) -> Self {
        Self {
            remaining: raw,
            left: item_count,
        }
    }
}

impl<'a> Iterator for VarPayloadDataItemIter<'a> {
    type Item = (VarPayloadDataItemRef<'a>, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        let left_before = self.left;
        match parse_var_payload_data_item(self.remaining) {
            Ok((mut rest, item)) => {
                // Skip padding byte between items if data length is odd
                if left_before > 1 && (item.data.len() & 1) == 1 {
                    if rest.is_empty() {
                        return None;
                    }
                    rest = &rest[1..];
                }
                self.remaining = rest;
                self.left -= 1;
                Some((item, rest))
            }
            Err(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarPayloadStatusItemRef {
    pub return_code: S7ReturnCode,
}

#[derive(Debug, Clone, Copy)]
pub struct VarPayloadStatusItemIter<'a> {
    remaining: &'a [u8],
    left: u8,
}

impl<'a> VarPayloadStatusItemIter<'a> {
    pub fn new(item_count: u8, raw: &'a [u8]) -> Self {
        Self {
            remaining: raw,
            left: item_count,
        }
    }
}

impl<'a> Iterator for VarPayloadStatusItemIter<'a> {
    type Item = (VarPayloadStatusItemRef, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        match parse_var_payload_status_item(self.remaining) {
            Ok((rest, item)) => {
                self.remaining = rest;
                self.left -= 1;
                Some((item, rest))
            }
            Err(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserDataPayloadItemRef<'a> {
    pub return_code: S7ReturnCode,
    pub transport_size: S7DataVariableType,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub struct UserDataPayloadItemIter<'a> {
    remaining: &'a [u8],
    left: u8,
}

impl<'a> UserDataPayloadItemIter<'a> {
    pub fn new(item_count: u8, raw: &'a [u8]) -> Self {
        Self {
            remaining: raw,
            left: item_count,
        }
    }
}

impl<'a> Iterator for UserDataPayloadItemIter<'a> {
    type Item = (UserDataPayloadItemRef<'a>, &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        match parse_user_data_payload_item(self.remaining) {
            Ok((rest, item)) => {
                self.remaining = rest;
                self.left -= 1;
                Some((item, rest))
            }
            Err(_) => None,
        }
    }
}

/// High-level classified user-data item kinds. The attached `data` slice is the raw payload
/// bytes of that item, i.e. the content governed by the discriminator. We keep it zero-copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S7UserDataItemKind<'a> {
    DiagnosticMessage {
        data: &'a [u8],
    },
    Alarm8 {
        data: &'a [u8],
    },
    Notify {
        data: &'a [u8],
    },
    AlarmAckInd {
        data: &'a [u8],
    },
    AlarmSQ {
        data: &'a [u8],
    },
    AlarmS {
        data: &'a [u8],
    },
    AlarmSC {
        data: &'a [u8],
    },
    Notify8 {
        data: &'a [u8],
    },
    CpuFunctionReadSzlNoDataRequest,
    CpuFunctionReadSzlRequest {
        data: &'a [u8],
    },
    CpuFunctionReadSzlResponse {
        data: &'a [u8],
    },
    CpuFunctionMsgSubscriptionRequest {
        data: &'a [u8],
    },
    CpuFunctionMsgSubscriptionResponse,
    CpuFunctionMsgSubscriptionSysResponse {
        data: &'a [u8],
    },
    CpuFunctionMsgSubscriptionAlarmResponse {
        data: &'a [u8],
    },
    CpuFunctionAlarmAckRequest {
        data: &'a [u8],
    },
    CpuFunctionAlarmAckErrorResponse,
    CpuFunctionAlarmAckResponse {
        data: &'a [u8],
    },
    CpuFunctionAlarmQueryRequest {
        data: &'a [u8],
    },
    CpuFunctionAlarmQueryResponse {
        data: &'a [u8],
    },
    ClkRequest,
    ClkResponse {
        data: &'a [u8],
    },
    ClkFRequest,
    ClkFResponse {
        data: &'a [u8],
    },
    ClkSetRequest {
        data: &'a [u8],
    },
    ClkSetResponse,
    /// Fallback if no specific mapping exists in our classifier
    Unknown {
        discriminator: CpuFunctionDiscriminator,
        data: &'a [u8],
    },
}

/// Discriminators derived from the CPU Functions parameter item (matches PLC4X usage)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuFunctionDiscriminator {
    /// cpuFunctionGroup (low nibble in PLC API)
    pub group: CpuFunctionGroup,
    /// cpuFunctionType (high nibble in PLC API)
    pub typ: CpuFunctionType,
    /// cpuSubfunction
    pub sub: u8,
}

/// Typed item view combining discriminator, generic item header and high-level kind
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S7UserDataTypedItemRef<'a> {
    /// Discriminators derived from the matching CPU Functions parameter item
    pub discriminator: CpuFunctionDiscriminator,
    /// Return code as carried by the item header
    pub return_code: S7ReturnCode,
    /// Transport size as carried by the item header
    pub transport_size: S7DataVariableType,
    /// Raw payload bytes of the item
    pub data: &'a [u8],
    /// High-level classified kind for convenience
    pub kind: S7UserDataItemKind<'a>,
}

/// Iterator zipping parameter items and payload items for CPU Service user-data
#[derive(Debug, Clone)]
pub struct UserDataTypedItemIter<'a, 'p> {
    param_iter: UserDataParamIter<'p>,
    payload_iter: UserDataPayloadItemIter<'a>,
}

impl<'a, 'p> UserDataTypedItemIter<'a, 'p> {
    pub fn new(item_count: u8, param_raw: &'p [u8], payload_raw: &'a [u8]) -> Self {
        Self {
            param_iter: UserDataParamIter::new(item_count, param_raw),
            payload_iter: UserDataPayloadItemIter::new(item_count, payload_raw),
        }
    }
}

impl<'a, 'p> Iterator for UserDataTypedItemIter<'a, 'p> {
    type Item = S7UserDataTypedItemRef<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        // Advance both iterators in lockstep. If either fails, stop.
        let (param, _) = self.param_iter.next()?;
        let (payload, _) = self.payload_iter.next()?;
        // Only CPU Functions items produce discriminators; others map to Unknown
        let disc = match param {
            S7UserDataParam::CpuFunctions {
                cpu_function_group,
                cpu_function_type,
                cpu_subfunction,
            } => Some(CpuFunctionDiscriminator {
                group: cpu_function_group,
                typ: cpu_function_type,
                sub: cpu_subfunction,
            }),
            S7UserDataParam::Unknown { .. } => None,
        };

        let (discriminator, kind) = match disc {
            Some(d) => {
                let k = classify_user_data_item(&d, payload.data);
                (d, k)
            }
            None => return None,
        };

        Some(S7UserDataTypedItemRef {
            discriminator,
            return_code: payload.return_code,
            transport_size: payload.transport_size,
            data: payload.data,
            kind,
        })
    }
}

fn parse_single_varspec(input: &[u8]) -> Result<(&[u8], S7VarSpec)> {
    let (i, syntax) =
        nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
    let _ = S7SyntaxId::try_from(syntax).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, _len) = nom_u8::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?; // length of following fields, typically 0x0A for S7Any
    let (i, transport_size) =
        nom_u8::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let transport_size =
        S7TransportSize::try_from(transport_size).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, count) =
        be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, db_number) =
        be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, area) = nom_u8::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let area = S7Area::try_from(area).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, address) =
        be_u32::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?; // actually 24-bit address; reading u32 then masking keeps code simple
    let byte_address = (address >> 8) & 0x00FF_FFFF;
    let bit_index = (address & 0xFF) as u8;
    Ok((
        i,
        S7VarSpec {
            transport_size,
            count,
            db_number,
            area,
            byte_address,
            bit_index,
        },
    ))
}

fn parse_single_userdata_item(input: &[u8]) -> Result<(&[u8], S7UserDataParam)> {
    // Layout matches PLC4X S7ParameterUserDataItemCPUFunctions:
    // itemType (u8), itemLength (u8), payload[itemLength]
    // payload: method (u8), cpuFunctionType (4 bits, high nibble) | cpuFunctionGroup (4 bits, low nibble),
    //          cpuSubfunction (u8), sequenceNumber (u8), ...optional fields we skip
    let (after_type, item_type) =
        nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
    let (payload_start, item_len) =
        nom_u8::<_, nom::error::Error<&[u8]>>(after_type).map_err(|_| Error::ErrInvalidFrame)?;
    let item_len = item_len as usize;
    if payload_start.len() < item_len {
        return Err(Error::ErrInvalidFrame);
    }
    let payload = &payload_start[..item_len];
    let rest = &payload_start[item_len..];

    if item_type != 0x12 {
        // Forward-compatible: skip unknown item payload and yield Unknown
        return Ok((
            rest,
            S7UserDataParam::Unknown {
                item_type,
                length: item_len as u8,
            },
        ));
    }
    if payload.len() < 4 {
        return Err(Error::ErrInvalidFrame);
    }
    // let method = payload[0]; // not used right now
    let t_g = payload[1];
    let cpu_function_type =
        CpuFunctionType::try_from(t_g >> 4).map_err(|_| Error::ErrInvalidFrame)?;
    let cpu_function_group =
        CpuFunctionGroup::try_from(t_g & 0x0F).map_err(|_| Error::ErrInvalidFrame)?;
    let cpu_subfunction = payload[2];
    // let sequence_number = payload[3]; // not used right now

    Ok((
        rest,
        S7UserDataParam::CpuFunctions {
            cpu_function_group,
            cpu_function_type,
            cpu_subfunction,
        },
    ))
}

fn parse_var_payload_data_item<'a>(
    input: &'a [u8],
) -> Result<(&'a [u8], VarPayloadDataItemRef<'a>)> {
    let (i, rc) =
        nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
    let return_code = S7ReturnCode::try_from(rc).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, ts) = nom_u8::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let transport_size = S7DataVariableType::try_from(ts).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, data_len_field) =
        be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let data_len = data_len_in_bytes(transport_size, data_len_field);
    if i.len() < data_len {
        return Err(Error::ErrInvalidFrame);
    }
    let (data, rest) = i.split_at(data_len);
    Ok((
        rest,
        VarPayloadDataItemRef {
            return_code,
            transport_size,
            data,
        },
    ))
}

fn parse_var_payload_status_item(input: &[u8]) -> Result<(&[u8], VarPayloadStatusItemRef)> {
    let (i, rc) =
        nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
    let return_code = S7ReturnCode::try_from(rc).map_err(|_| Error::ErrInvalidFrame)?;
    Ok((i, VarPayloadStatusItemRef { return_code }))
}

fn parse_user_data_payload_item<'a>(
    input: &'a [u8],
) -> Result<(&'a [u8], UserDataPayloadItemRef<'a>)> {
    let (i, rc) =
        nom_u8::<_, nom::error::Error<&[u8]>>(input).map_err(|_| Error::ErrInvalidFrame)?;
    let return_code = S7ReturnCode::try_from(rc).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, ts) = nom_u8::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let transport_size = S7DataVariableType::try_from(ts).map_err(|_| Error::ErrInvalidFrame)?;
    let (i, data_len_field) =
        be_u16::<_, nom::error::Error<&[u8]>>(i).map_err(|_| Error::ErrInvalidFrame)?;
    let data_len = data_len_in_bytes(transport_size, data_len_field);
    if i.len() < data_len {
        return Err(Error::ErrInvalidFrame);
    }
    let (data, rest) = i.split_at(data_len);
    Ok((
        rest,
        UserDataPayloadItemRef {
            return_code,
            transport_size,
            data,
        },
    ))
}

/// Classify a user-data payload item into a high-level kind using PLC4X discriminators.
fn classify_user_data_item<'a>(
    disc: &CpuFunctionDiscriminator,
    data: &'a [u8],
) -> S7UserDataItemKind<'a> {
    use super::types::{CpuFunctionGroup::*, CpuFunctionType::*};

    let len = data.len();
    match (disc.group, disc.typ, disc.sub) {
        // CpuFunctions + IndicationPush
        (CpuFunctions, IndicationPush, 0x03) => {
            return S7UserDataItemKind::DiagnosticMessage { data }
        }
        (CpuFunctions, IndicationPush, 0x05) => return S7UserDataItemKind::Alarm8 { data },
        (CpuFunctions, IndicationPush, 0x06) => return S7UserDataItemKind::Notify { data },
        (CpuFunctions, IndicationPush, 0x0C) => return S7UserDataItemKind::AlarmAckInd { data },
        (CpuFunctions, IndicationPush, 0x11) => return S7UserDataItemKind::AlarmSQ { data },
        (CpuFunctions, IndicationPush, 0x12) => return S7UserDataItemKind::AlarmS { data },
        (CpuFunctions, IndicationPush, 0x13) => return S7UserDataItemKind::AlarmSC { data },
        (CpuFunctions, IndicationPush, 0x16) => return S7UserDataItemKind::Notify8 { data },

        // CpuFunctions + Request
        (CpuFunctions, Request, 0x01) if len == 0 => {
            return S7UserDataItemKind::CpuFunctionReadSzlNoDataRequest
        }
        (CpuFunctions, Request, 0x01) => {
            return S7UserDataItemKind::CpuFunctionReadSzlRequest { data }
        }
        (CpuFunctions, Request, 0x02) => {
            return S7UserDataItemKind::CpuFunctionMsgSubscriptionRequest { data }
        }
        (CpuFunctions, Request, 0x0B) => {
            return S7UserDataItemKind::CpuFunctionAlarmAckRequest { data }
        }
        (CpuFunctions, Request, 0x13) => {
            return S7UserDataItemKind::CpuFunctionAlarmQueryRequest { data }
        }

        // CpuFunctions + Response
        (CpuFunctions, Response, 0x01) => {
            return S7UserDataItemKind::CpuFunctionReadSzlResponse { data }
        }
        (CpuFunctions, Response, 0x02) => match len {
            0 => return S7UserDataItemKind::CpuFunctionMsgSubscriptionResponse,
            2 => return S7UserDataItemKind::CpuFunctionMsgSubscriptionSysResponse { data },
            5 => return S7UserDataItemKind::CpuFunctionMsgSubscriptionAlarmResponse { data },
            _ => {}
        },
        (CpuFunctions, Response, 0x0B) => match len {
            0 => return S7UserDataItemKind::CpuFunctionAlarmAckErrorResponse,
            _ => return S7UserDataItemKind::CpuFunctionAlarmAckResponse { data },
        },
        (CpuFunctions, Response, 0x13) => {
            return S7UserDataItemKind::CpuFunctionAlarmQueryResponse { data }
        }

        // TimeFunctions + Request
        (TimeFunctions, Request, 0x01) => return S7UserDataItemKind::ClkRequest,
        (TimeFunctions, Request, 0x03) => return S7UserDataItemKind::ClkFRequest,
        (TimeFunctions, Request, 0x04) => return S7UserDataItemKind::ClkSetRequest { data },

        // TimeFunctions + Response
        (TimeFunctions, Response, 0x01) => return S7UserDataItemKind::ClkResponse { data },
        (TimeFunctions, Response, 0x03) => return S7UserDataItemKind::ClkFResponse { data },
        (TimeFunctions, Response, 0x04) => return S7UserDataItemKind::ClkSetResponse,

        _ => {}
    }
    S7UserDataItemKind::Unknown {
        discriminator: *disc,
        data,
    }
}

fn data_len_in_bytes(typ: S7DataVariableType, len_field: u16) -> usize {
    match typ {
        // These declare length in bits, convert to ceil(bytes)
        S7DataVariableType::Null
        | S7DataVariableType::ByteWordDWord
        | S7DataVariableType::Integer => (len_field as usize) / 8,
        // These declare length in bytes directly
        S7DataVariableType::Bit
        | S7DataVariableType::DInteger
        | S7DataVariableType::Real
        | S7DataVariableType::OctetString => len_field as usize,
    }
}
