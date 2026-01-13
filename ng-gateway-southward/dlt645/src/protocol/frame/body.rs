use super::defs::Dl645Address;
use super::Dl645Function;
use crate::protocol::error::ProtocolError;
use crate::types::Dl645Version;
use bytes::{BufMut, Bytes};
use ng_gateway_sdk::{WireDecode, WireEncode};

/// Context for encoding/decoding DL/T 645 frames
#[derive(Debug, Clone)]
pub struct Dl645CodecContext {
    pub version: Dl645Version,
    /// Function code is required for decoding the body, as the body structure depends on it.
    pub function_code: Option<Dl645Function>,
    /// Indicates if the frame is an exception response (D6=1).
    /// If true, the body is treated as a raw payload (exception code).
    pub is_exception: bool,
}

impl Dl645CodecContext {
    pub fn new(version: Dl645Version) -> Self {
        Self {
            version,
            function_code: None,
            is_exception: false,
        }
    }

    pub fn with_function(mut self, function: Dl645Function) -> Self {
        self.function_code = Some(function);
        self
    }

    pub fn with_exception(mut self, is_exception: bool) -> Self {
        self.is_exception = is_exception;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Dl645Body {
    ReadData(ReadDataBody),
    WriteData(WriteDataBody),
    WriteAddress(WriteAddressBody),
    BroadcastTimeSync(BroadcastTimeSyncBody),
    Freeze(FreezeBody),
    UpdateBaudRate(UpdateBaudRateBody),
    ModifyPassword(ModifyPasswordBody),
    ClearMaxDemand(ClearMaxDemandBody),
    ClearMeter(ClearMeterBody),
    ClearEvents(ClearEventsBody),
    /// Represents a raw payload, used for unknown functions, exception responses, or when structured parsing is not possible.
    Raw(Bytes),
}

impl Dl645Body {
    pub fn function_code(&self) -> Dl645Function {
        match self {
            Dl645Body::ReadData(_) => Dl645Function::ReadData,
            Dl645Body::WriteData(_) => Dl645Function::WriteData,
            Dl645Body::WriteAddress(_) => Dl645Function::WriteAddress,
            Dl645Body::BroadcastTimeSync(_) => Dl645Function::BroadcastTimeSync,
            Dl645Body::Freeze(_) => Dl645Function::Freeze,
            Dl645Body::UpdateBaudRate(_) => Dl645Function::UpdateBaudRate,
            Dl645Body::ModifyPassword(_) => Dl645Function::ModifyPassword,
            Dl645Body::ClearMaxDemand(_) => Dl645Function::ClearMaxDemand,
            Dl645Body::ClearMeter(_) => Dl645Function::ClearMeter,
            Dl645Body::ClearEvents(_) => Dl645Function::ClearEvents,
            Dl645Body::Raw(_) => Dl645Function::Reserved,
        }
    }

    /// Extract the raw data payload from the body, if applicable.
    /// This is useful for aggregating multi-frame responses.
    pub fn data_payload(&self) -> Option<&[u8]> {
        match self {
            Dl645Body::ReadData(b) => Some(b.data.as_ref()),
            Dl645Body::Raw(b) => Some(b.as_ref()),
            // For other bodies, the "data" concept is less clear or they are not used in multi-frame reads.
            _ => None,
        }
    }
}

impl WireEncode for Dl645Body {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match self {
            Dl645Body::ReadData(b) => b.encoded_len(ctx),
            Dl645Body::WriteData(b) => b.encoded_len(ctx),
            Dl645Body::WriteAddress(b) => b.encoded_len(ctx),
            Dl645Body::BroadcastTimeSync(b) => b.encoded_len(ctx),
            Dl645Body::Freeze(b) => b.encoded_len(ctx),
            Dl645Body::UpdateBaudRate(b) => b.encoded_len(ctx),
            Dl645Body::ModifyPassword(b) => b.encoded_len(ctx),
            Dl645Body::ClearMaxDemand(b) => b.encoded_len(ctx),
            Dl645Body::ClearMeter(b) => b.encoded_len(ctx),
            Dl645Body::ClearEvents(b) => b.encoded_len(ctx),
            Dl645Body::Raw(b) => b.len(),
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        match self {
            Dl645Body::ReadData(b) => b.encode_to(dst, ctx),
            Dl645Body::WriteData(b) => b.encode_to(dst, ctx),
            Dl645Body::WriteAddress(b) => b.encode_to(dst, ctx),
            Dl645Body::BroadcastTimeSync(b) => b.encode_to(dst, ctx),
            Dl645Body::Freeze(b) => b.encode_to(dst, ctx),
            Dl645Body::UpdateBaudRate(b) => b.encode_to(dst, ctx),
            Dl645Body::ModifyPassword(b) => b.encode_to(dst, ctx),
            Dl645Body::ClearMaxDemand(b) => b.encode_to(dst, ctx),
            Dl645Body::ClearMeter(b) => b.encode_to(dst, ctx),
            Dl645Body::ClearEvents(b) => b.encode_to(dst, ctx),
            Dl645Body::Raw(b) => {
                dst.put_slice(b);
                Ok(())
            }
        }
    }
}

impl WireDecode for Dl645Body {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        // If it's an exception frame, return Raw immediately
        if ctx.is_exception {
            let raw_bytes = if !input.is_empty() {
                parent.slice_ref(input)
            } else {
                Bytes::new()
            };
            return Ok((&[], Dl645Body::Raw(raw_bytes)));
        }

        let function = ctx.function_code.ok_or(ProtocolError::InvalidFrame(
            "Function code missing in context for body decode".into(),
        ))?;

        match function {
            Dl645Function::ReadData => {
                let (rem, body) = ReadDataBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::ReadData(body)))
            }
            Dl645Function::WriteData => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = WriteDataBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::WriteData(body)))
            }
            Dl645Function::WriteAddress => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = WriteAddressBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::WriteAddress(body)))
            }
            Dl645Function::BroadcastTimeSync => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = BroadcastTimeSyncBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::BroadcastTimeSync(body)))
            }
            Dl645Function::Freeze => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = FreezeBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::Freeze(body)))
            }
            Dl645Function::UpdateBaudRate => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = UpdateBaudRateBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::UpdateBaudRate(body)))
            }
            Dl645Function::ModifyPassword => {
                let raw_bytes = if !input.is_empty() {
                    parent.slice_ref(input)
                } else {
                    Bytes::new()
                };
                Ok((&[], Dl645Body::Raw(raw_bytes)))
            }
            Dl645Function::ClearMaxDemand => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = ClearMaxDemandBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::ClearMaxDemand(body)))
            }
            Dl645Function::ClearMeter => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = ClearMeterBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::ClearMeter(body)))
            }
            Dl645Function::ClearEvents => {
                if input.is_empty() {
                    return Ok((input, Dl645Body::Raw(Bytes::new())));
                }
                let (rem, body) = ClearEventsBody::parse(input, parent, ctx)?;
                Ok((rem, Dl645Body::ClearEvents(body)))
            }
            _ => {
                let raw_bytes = if !input.is_empty() {
                    parent.slice_ref(input)
                } else {
                    Bytes::new()
                };
                Ok((&[], Dl645Body::Raw(raw_bytes)))
            }
        }
    }
}

// ... rest of the file (ReadDataBody etc.)
// Make sure to include the modified ReadDataBody and others as before.
// Re-inserting helpers and structs to ensure file completeness

fn get_di_bytes(di: u32, version: Dl645Version) -> (Vec<u8>, usize) {
    let di_bytes_full = [
        (di & 0xFF) as u8,
        ((di >> 8) & 0xFF) as u8,
        ((di >> 16) & 0xFF) as u8,
        ((di >> 24) & 0xFF) as u8,
    ];
    let len = match version {
        Dl645Version::V1997 => 2,
        _ => 4,
    };
    (di_bytes_full[..len].to_vec(), len)
}

fn parse_di(input: &[u8], version: Dl645Version) -> Result<(&[u8], u32), ProtocolError> {
    let len = match version {
        Dl645Version::V1997 => 2,
        _ => 4,
    };
    if input.len() < len {
        return Err(ProtocolError::InvalidFrame(
            "Insufficient bytes for DI".into(),
        ));
    }
    let (bytes, rem) = input.split_at(len);
    let mut di: u32 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        di |= (b as u32) << (8 * i);
    }
    Ok((rem, di))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReadDataBody {
    pub di: u32,
    pub data: Bytes,
}

impl WireEncode for ReadDataBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        let di_len = match ctx.version {
            Dl645Version::V1997 => 2,
            _ => 4,
        };
        di_len + self.data.len()
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        let (bytes, _) = get_di_bytes(self.di, ctx.version);
        dst.put_slice(&bytes);
        dst.put_slice(&self.data);
        Ok(())
    }
}

impl WireDecode for ReadDataBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        let (rem, di) = parse_di(input, ctx.version)?;

        let data = if !rem.is_empty() {
            parent.slice_ref(rem)
        } else {
            Bytes::new()
        };

        Ok((&[], ReadDataBody { di, data }))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteDataBody {
    pub di: u32,
    pub value: Bytes,
    pub password: u32,
    pub operator_code: u32,
}

impl WireEncode for WriteDataBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match ctx.version {
            Dl645Version::V1997 => 2 + self.value.len(),
            _ => 4 + 4 + 4 + self.value.len(),
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        match ctx.version {
            Dl645Version::V1997 => {
                let (di_bytes, _) = get_di_bytes(self.di, ctx.version);
                dst.put_slice(&di_bytes);
                dst.put_slice(&self.value);
            }
            _ => {
                let (di_bytes, _) = get_di_bytes(self.di, ctx.version);
                dst.put_slice(&di_bytes);
                dst.put_slice(&self.password.to_le_bytes());
                dst.put_slice(&self.operator_code.to_le_bytes());
                dst.put_slice(&self.value);
            }
        }
        Ok(())
    }
}

impl WireDecode for WriteDataBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        let (rem, di) = parse_di(input, ctx.version)?;
        match ctx.version {
            Dl645Version::V1997 => {
                let value = if !rem.is_empty() {
                    parent.slice_ref(rem)
                } else {
                    Bytes::new()
                };
                Ok((
                    &[],
                    WriteDataBody {
                        di,
                        value,
                        password: 0,
                        operator_code: 0,
                    },
                ))
            }
            _ => {
                if rem.len() < 8 {
                    return Err(ProtocolError::InvalidFrame(
                        "Insufficient bytes for WriteData header".into(),
                    ));
                }
                let (pw_bytes, rem) = rem.split_at(4);
                let (op_bytes, rem) = rem.split_at(4);

                let password = u32::from_le_bytes(pw_bytes.try_into().unwrap());
                let operator_code = u32::from_le_bytes(op_bytes.try_into().unwrap());

                let value = if !rem.is_empty() {
                    parent.slice_ref(rem)
                } else {
                    Bytes::new()
                };

                Ok((
                    &[],
                    WriteDataBody {
                        di,
                        value,
                        password,
                        operator_code,
                    },
                ))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriteAddressBody {
    pub new_address: Dl645Address,
}

impl WireEncode for WriteAddressBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        6
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<(), Self::Error> {
        dst.put_slice(&self.new_address.0);
        Ok(())
    }
}

impl WireDecode for WriteAddressBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if input.len() < 6 {
            return Err(ProtocolError::InvalidFrame(
                "Insufficient bytes for WriteAddress".into(),
            ));
        }
        let (addr_bytes, rem) = input.split_at(6);
        let mut arr = [0u8; 6];
        arr.copy_from_slice(addr_bytes);
        Ok((
            rem,
            WriteAddressBody {
                new_address: Dl645Address(arr),
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastTimeSyncBody {
    pub timestamp: Bytes,
}

impl WireEncode for BroadcastTimeSyncBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        self.timestamp.len()
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<(), Self::Error> {
        dst.put_slice(&self.timestamp);
        Ok(())
    }
}

impl WireDecode for BroadcastTimeSyncBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if input.len() < 6 {
            return Err(ProtocolError::InvalidFrame(
                "Insufficient bytes for TimeSync".into(),
            ));
        }
        let (ts, rem) = input.split_at(6);
        let timestamp = if !ts.is_empty() {
            parent.slice_ref(ts)
        } else {
            Bytes::new()
        };
        Ok((rem, BroadcastTimeSyncBody { timestamp }))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FreezeBody {
    pub pattern: Bytes,
}

impl WireEncode for FreezeBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        self.pattern.len()
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<(), Self::Error> {
        dst.put_slice(&self.pattern);
        Ok(())
    }
}

impl WireDecode for FreezeBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if input.len() < 4 {
            return Err(ProtocolError::InvalidFrame(
                "Insufficient bytes for Freeze".into(),
            ));
        }
        let (pat, rem) = input.split_at(4);
        let pattern = if !pat.is_empty() {
            parent.slice_ref(pat)
        } else {
            Bytes::new()
        };
        Ok((rem, FreezeBody { pattern }))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateBaudRateBody {
    pub code: u8,
}

impl WireEncode for UpdateBaudRateBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        1
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<(), Self::Error> {
        dst.put_u8(self.code);
        Ok(())
    }
}

impl WireDecode for UpdateBaudRateBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if input.is_empty() {
            return Err(ProtocolError::InvalidFrame(
                "Insufficient bytes for UpdateBaudRate".into(),
            ));
        }
        let (code, rem) = input.split_at(1);
        Ok((rem, UpdateBaudRateBody { code: code[0] }))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModifyPasswordBody {
    pub di: Option<u32>,
    pub old_password: u32,
    pub new_password: u32,
}

impl WireEncode for ModifyPasswordBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match ctx.version {
            Dl645Version::V1997 => 8,
            _ => 12,
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        match ctx.version {
            Dl645Version::V1997 => {
                dst.put_slice(&self.old_password.to_le_bytes());
                dst.put_slice(&self.new_password.to_le_bytes());
            }
            _ => {
                let di = self.di.ok_or(ProtocolError::InvalidFrame(
                    "DI is required for ModifyPassword V2007".into(),
                ))?; // Default to permission level 02 if not provided
                let (di_bytes, _) = get_di_bytes(di, ctx.version);
                dst.put_slice(&di_bytes);
                dst.put_slice(&self.old_password.to_le_bytes());
                dst.put_slice(&self.new_password.to_le_bytes());
            }
        }
        Ok(())
    }
}

impl WireDecode for ModifyPasswordBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        match ctx.version {
            Dl645Version::V1997 => {
                if input.len() < 8 {
                    return Err(ProtocolError::InvalidFrame(
                        "Insufficient bytes for ModifyPassword V1997".into(),
                    ));
                }
                let (old_bytes, rem) = input.split_at(4);
                let (new_bytes, rem) = rem.split_at(4);

                let old_password = u32::from_le_bytes(old_bytes.try_into().unwrap());
                let new_password = u32::from_le_bytes(new_bytes.try_into().unwrap());

                Ok((
                    rem,
                    ModifyPasswordBody {
                        di: None,
                        old_password,
                        new_password,
                    },
                ))
            }
            _ => {
                if input.len() < 12 {
                    return Err(ProtocolError::InvalidFrame(
                        "Insufficient bytes for ModifyPassword".into(),
                    ));
                }
                let (di_bytes, rem) = input.split_at(4);
                let (old_bytes, rem) = rem.split_at(4);
                let (new_bytes, rem) = rem.split_at(4);

                let di = u32::from_le_bytes(di_bytes.try_into().unwrap());
                let old_password = u32::from_le_bytes(old_bytes.try_into().unwrap());
                let new_password = u32::from_le_bytes(new_bytes.try_into().unwrap());

                Ok((
                    rem,
                    ModifyPasswordBody {
                        di: Some(di),
                        old_password,
                        new_password,
                    },
                ))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClearMaxDemandBody {
    pub password: u32,
    pub operator_code: u32,
}

impl WireEncode for ClearMaxDemandBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match ctx.version {
            Dl645Version::V1997 => 0,
            _ => 8,
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        if !matches!(ctx.version, Dl645Version::V1997) {
            dst.put_slice(&self.password.to_le_bytes());
            dst.put_slice(&self.operator_code.to_le_bytes());
        }
        Ok(())
    }
}

impl WireDecode for ClearMaxDemandBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if matches!(ctx.version, Dl645Version::V1997) {
            Ok((
                input,
                ClearMaxDemandBody {
                    password: 0,
                    operator_code: 0,
                },
            ))
        } else {
            if input.len() < 8 {
                return Err(ProtocolError::InvalidFrame(
                    "Insufficient bytes for ClearMaxDemand".into(),
                ));
            }
            let (pw_bytes, rem) = input.split_at(4);
            let (op_bytes, rem) = rem.split_at(4);

            let password = u32::from_le_bytes(pw_bytes.try_into().unwrap());
            let operator_code = u32::from_le_bytes(op_bytes.try_into().unwrap());
            Ok((
                rem,
                ClearMaxDemandBody {
                    password,
                    operator_code,
                },
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClearMeterBody {
    pub password: u32,
    pub operator_code: u32,
}

impl WireEncode for ClearMeterBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match ctx.version {
            Dl645Version::V1997 => 0,
            _ => 8,
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        if !matches!(ctx.version, Dl645Version::V1997) {
            dst.put_slice(&self.password.to_le_bytes());
            dst.put_slice(&self.operator_code.to_le_bytes());
        }
        Ok(())
    }
}

impl WireDecode for ClearMeterBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if matches!(ctx.version, Dl645Version::V1997) {
            Ok((
                input,
                ClearMeterBody {
                    password: 0,
                    operator_code: 0,
                },
            ))
        } else {
            if input.len() < 8 {
                return Err(ProtocolError::InvalidFrame(
                    "Insufficient bytes for ClearMeter".into(),
                ));
            }
            let (pw_bytes, rem) = input.split_at(4);
            let (op_bytes, rem) = rem.split_at(4);

            let password = u32::from_le_bytes(pw_bytes.try_into().unwrap());
            let operator_code = u32::from_le_bytes(op_bytes.try_into().unwrap());
            Ok((
                rem,
                ClearMeterBody {
                    password,
                    operator_code,
                },
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClearEventsBody {
    pub di: u32,
    pub password: u32,
    pub operator_code: u32,
}

impl WireEncode for ClearEventsBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        match ctx.version {
            Dl645Version::V1997 => 4,
            _ => 12,
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        let di_bytes = [
            (self.di & 0xFF) as u8,
            ((self.di >> 8) & 0xFF) as u8,
            ((self.di >> 16) & 0xFF) as u8,
            ((self.di >> 24) & 0xFF) as u8,
        ];

        match ctx.version {
            Dl645Version::V1997 => {
                dst.put_slice(&di_bytes);
            }
            _ => {
                // For V2007 Clear Events, the order is Password -> Operator -> DI
                dst.put_slice(&self.password.to_le_bytes());
                dst.put_slice(&self.operator_code.to_le_bytes());
                dst.put_slice(&di_bytes);
            }
        }
        Ok(())
    }
}

impl WireDecode for ClearEventsBody {
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        match ctx.version {
            Dl645Version::V1997 => {
                if input.len() < 4 {
                    return Err(ProtocolError::InvalidFrame(
                        "Insufficient bytes for ClearEvents DI".into(),
                    ));
                }
                let (di_bytes, rem) = input.split_at(4);
                let di = u32::from_le_bytes(di_bytes.try_into().unwrap());
                Ok((
                    rem,
                    ClearEventsBody {
                        di,
                        password: 0,
                        operator_code: 0,
                    },
                ))
            }
            _ => {
                // For V2007 Clear Events, the order is Password -> Operator -> DI
                // Total 12 bytes
                if input.len() < 12 {
                    return Err(ProtocolError::InvalidFrame(
                        "Insufficient bytes for ClearEvents PW/OP/DI".into(),
                    ));
                }
                let (pw_bytes, rem) = input.split_at(4);
                let (op_bytes, rem) = rem.split_at(4);
                let (di_bytes, rem) = rem.split_at(4);

                let password = u32::from_le_bytes(pw_bytes.try_into().unwrap());
                let operator_code = u32::from_le_bytes(op_bytes.try_into().unwrap());
                let di = u32::from_le_bytes(di_bytes.try_into().unwrap());

                Ok((
                    rem,
                    ClearEventsBody {
                        di,
                        password,
                        operator_code,
                    },
                ))
            }
        }
    }
}
