use super::defs::*;
use super::Cjt188CodecContext;
use crate::protocol::error::ProtocolError;
use crate::types::Cjt188Version;
use bytes::{BufMut, Bytes};
use ng_gateway_sdk::{WireDecode, WireEncode};
use std::borrow::Cow;

/// Body of CJ/T 188 Frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Cjt188Body {
    /// Raw bytes (fallback or unknown).
    Raw(Bytes),

    /// Read Data Request.
    /// Frame: DI(2) + SER(1)
    ///
    /// Note: In practice, both CJ/T 188-2004 and CJ/T 188-2018 devices commonly require a
    /// 1-byte sequence domain (SER/SEQ) to correlate request and response frames.
    ReadData {
        di: DataIdentifier,
        /// Frame sequence number (SER/SEQ), 1 byte.
        serial: u8,
    },

    /// Read Data Response.
    /// Frame: DI(2) + SER(1) + DATA(N)
    ReadDataResponse {
        di: DataIdentifier,
        serial: u8,
        value_bytes: Bytes,
    },

    /// Write Data Request (e.g. Valve Control).
    /// Frame: DI(2) + SER(1) + DATA(N)
    WriteData {
        di: DataIdentifier,
        /// Frame sequence number (SER/SEQ), 1 byte.
        serial: u8,
        value_bytes: Bytes,
    },

    /// Write Data Response.
    /// Frame: DI(2) + SER(1) + ST(1)
    WriteDataResponse {
        di: DataIdentifier,
        serial: u8,
        status: u8,
    },

    /// Write Motor Sync Data Request (Machine-Electric Synchronization).
    /// Function Code: 16H.
    /// Frame: DI(2) + SER(1) + DATA(N)
    WriteMotorSync {
        di: DataIdentifier,
        /// Frame sequence number (SER/SEQ), 1 byte.
        serial: u8,
        value_bytes: Bytes,
    },

    /// Write Motor Sync Data Response.
    /// Function Code: 16H.
    /// Frame: DI(2) + SER(1) + ST(1)
    WriteMotorSyncResponse {
        di: DataIdentifier,
        serial: u8,
        status: u8,
    },

    /// Read Address Request.
    /// Frame: DI(2) + SER(1) (Usually DI=810A)
    ReadAddr { di: DataIdentifier, serial: u8 },

    /// Read Address Response.
    /// Frame: DI(2) + SER(1) + A0..A6 (7 bytes)
    ReadAddrResponse {
        di: DataIdentifier,
        serial: u8,
        address: Cjt188Address,
    },

    /// Write Address Request.
    /// Frame: DI(2) + SER(1) + A0..A6
    WriteAddr {
        di: DataIdentifier,
        /// Frame sequence number (SER/SEQ), 1 byte.
        serial: u8,
        new_address: Cjt188Address,
    },

    /// Write Address Response.
    /// Frame: DI(2) + SER(1) + ST(1)
    WriteAddrResponse {
        di: DataIdentifier,
        serial: u8,
        status: u8,
    },

    /// Exception / Error Response.
    /// Frame: Control D6=1.
    /// Data: Status Byte (1 byte) describing the error.
    ExceptionResponse {
        original_func: FunctionCode,
        error_status: u8,
    },
}

impl Cjt188Body {
    pub fn function_code(&self) -> FunctionCode {
        match self {
            Self::Raw(_) => FunctionCode::Unknown,
            Self::ReadData { .. } => FunctionCode::ReadData,
            Self::ReadDataResponse { .. } => FunctionCode::ReadData,
            Self::WriteData { .. } => FunctionCode::WriteData,
            Self::WriteDataResponse { .. } => FunctionCode::WriteData,
            Self::WriteMotorSync { .. } => FunctionCode::WriteMotorSync,
            Self::WriteMotorSyncResponse { .. } => FunctionCode::WriteMotorSync,
            Self::ReadAddr { .. } => FunctionCode::ReadAddr,
            Self::ReadAddrResponse { .. } => FunctionCode::ReadAddr,
            Self::WriteAddr { .. } => FunctionCode::WriteAddr,
            Self::WriteAddrResponse { .. } => FunctionCode::WriteAddr,
            Self::ExceptionResponse { original_func, .. } => *original_func,
        }
    }

    pub fn data_payload(&self) -> Option<Cow<'_, [u8]>> {
        match self {
            Self::Raw(b) => Some(Cow::Borrowed(b)),
            Self::ReadDataResponse { value_bytes, .. } => Some(Cow::Borrowed(value_bytes)),
            // ReadAddrResponse carries the address in its body, which is effectively its payload.
            // While not "measurement data", generic readers might expect to access it.
            Self::ReadAddrResponse { address, .. } => Some(Cow::Owned(address.to_bytes().to_vec())),
            // Write responses typically only carry status codes, handled via status check,
            // so returning None for payload is appropriate.
            _ => None,
        }
    }
}

impl WireEncode for Cjt188Body {
    type Error = ProtocolError;
    type Context = Cjt188CodecContext;

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        match self {
            Self::Raw(b) => b.len(),
            Self::ReadData { .. } => 3, // DI(2) + SER(1)
            Self::ReadDataResponse { value_bytes, .. } => 3 + value_bytes.len(),
            Self::WriteData { value_bytes, .. } => 3 + value_bytes.len(), // DI(2) + SER(1) + DATA(N)
            Self::WriteDataResponse { .. } => 4,                          // DI(2) + SER(1) + ST(1)
            Self::WriteMotorSync { value_bytes, .. } => 3 + value_bytes.len(), // DI(2) + SER(1) + DATA(N)
            Self::WriteMotorSyncResponse { .. } => 4, // DI(2) + SER(1) + ST(1)
            Self::ReadAddr { .. } => 3,               // DI(2) + SER(1)
            Self::ReadAddrResponse { .. } => 10,      // DI(2) + SER(1) + ADDR(7)
            Self::WriteAddr { .. } => 10,             // DI(2) + SER(1) + ADDR(7)
            Self::WriteAddrResponse { .. } => 4,      // DI(2) + SER(1) + ST(1)
            Self::ExceptionResponse { .. } => 1,      // Status byte only
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        match self {
            Self::Raw(b) => dst.put_slice(b),
            Self::ReadData { di, serial } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
            }
            Self::ReadDataResponse {
                di,
                serial,
                value_bytes,
            } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_slice(value_bytes);
            }
            Self::WriteData {
                di,
                serial,
                value_bytes,
            } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_slice(value_bytes);
            }
            Self::WriteDataResponse { di, serial, status } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_u8(*status);
            }
            Self::WriteMotorSync {
                di,
                serial,
                value_bytes,
            } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_slice(value_bytes);
            }
            Self::WriteMotorSyncResponse { di, serial, status } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_u8(*status);
            }
            Self::ReadAddr { di, serial } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
            }
            Self::ReadAddrResponse {
                di,
                serial,
                address,
            } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_slice(&address.to_bytes());
            }
            Self::WriteAddr {
                di,
                serial,
                new_address,
            } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_slice(&new_address.to_bytes());
            }
            Self::WriteAddrResponse { di, serial, status } => {
                put_di(dst, u16::from(*di), ctx.version);
                dst.put_u8(*serial);
                dst.put_u8(*status);
            }
            Self::ExceptionResponse { error_status, .. } => {
                dst.put_u8(*error_status);
            }
        }
        Ok(())
    }
}

impl WireDecode for Cjt188Body {
    type Error = ProtocolError;
    type Context = Cjt188CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        let func = ctx.function_code.unwrap_or(FunctionCode::Unknown);

        // Check for Exception Response (D6 = 1)
        if ctx.is_error {
            // Error response body is usually 1 byte (Status).
            // Some devices might return more or less, but standard implies status byte.
            if input.is_empty() {
                // Should we fail or return 0? Standard says "Data field: error status byte"
                return Err(ProtocolError::FrameTooShort(
                    "Exception response missing status byte".into(),
                ));
            }
            let error_status = input[0];
            return Ok((
                &input[1..],
                Self::ExceptionResponse {
                    original_func: func,
                    error_status,
                },
            ));
        }

        match func {
            FunctionCode::ReadData => {
                // Read Data Response: DI(2) + SER(1) + DATA(N)
                if input.len() < 3 {
                    return Err(ProtocolError::FrameTooShort(
                        "Body too short for ReadData Response".into(),
                    ));
                }

                let di_raw = get_di(&input[0..2], ctx.version);
                let di = DataIdentifier::from(di_raw);
                let serial = input[2];
                let value_bytes = Bytes::copy_from_slice(&input[3..]);

                Ok((
                    &[],
                    Self::ReadDataResponse {
                        di,
                        serial,
                        value_bytes,
                    },
                ))
            }
            FunctionCode::WriteData => {
                // Write Data Response: DI(2) + SER(1) + ST(1)
                if input.len() < 4 {
                    return Err(ProtocolError::FrameTooShort(
                        "Body too short for WriteData Response".into(),
                    ));
                }
                let di_raw = get_di(&input[0..2], ctx.version);
                let di = DataIdentifier::from(di_raw);
                let serial = input[2];
                let status = input[3];

                Ok((&[], Self::WriteDataResponse { di, serial, status }))
            }
            FunctionCode::WriteMotorSync => {
                // Write Motor Sync Response: DI(2) + SER(1) + ST(1)
                if input.len() < 4 {
                    return Err(ProtocolError::FrameTooShort(
                        "Body too short for WriteMotorSync Response".into(),
                    ));
                }
                let di_raw = get_di(&input[0..2], ctx.version);
                let di = DataIdentifier::from(di_raw);
                let serial = input[2];
                let status = input[3];

                Ok((&[], Self::WriteMotorSyncResponse { di, serial, status }))
            }
            FunctionCode::ReadAddr => {
                // Response: DI(2) + SER(1) + Address (7 bytes)
                if input.len() < 10 {
                    return Err(ProtocolError::FrameTooShort(
                        "Body too short for ReadAddr Response (min 10)".into(),
                    ));
                }
                let di_raw = get_di(&input[0..2], ctx.version);
                let di = DataIdentifier::from(di_raw);
                let serial = input[2];
                let address = Cjt188Address::from_bytes(&input[3..10])?;
                Ok((
                    &[],
                    Self::ReadAddrResponse {
                        di,
                        serial,
                        address,
                    },
                ))
            }
            FunctionCode::WriteAddr => {
                // Write Address Response: DI(2) + SER(1) + ST(1)
                if input.len() == 4 {
                    let di_raw = get_di(&input[0..2], ctx.version);
                    let di = DataIdentifier::from(di_raw);
                    let serial = input[2];
                    let status = input[3];
                    Ok((&[], Self::WriteAddrResponse { di, serial, status }))
                } else {
                    Err(ProtocolError::FrameTooShort(
                        "Body length mismatch for WriteAddr Response (expected 4)".into(),
                    ))
                }
            }
            _ => {
                // Default fallback
                Ok((&[], Self::Raw(Bytes::copy_from_slice(input))))
            }
        }
    }
}

fn put_di<B: BufMut>(dst: &mut B, di: u16, version: Cjt188Version) {
    match version {
        Cjt188Version::V2004 => dst.put_u16(di),    // Big Endian
        Cjt188Version::V2018 => dst.put_u16_le(di), // Little Endian
    }
}

fn get_di(slice: &[u8], version: Cjt188Version) -> u16 {
    match version {
        Cjt188Version::V2004 => u16::from_be_bytes([slice[0], slice[1]]),
        Cjt188Version::V2018 => u16::from_le_bytes([slice[0], slice[1]]),
    }
}
