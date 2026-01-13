use crate::protocol::{
    error::ProtocolError,
    frame::defs::{Cjt188Address, ControlWord, FunctionCode, MeterType},
};
use crate::types::Cjt188Version;
use bytes::{BufMut, Bytes, BytesMut};
use ng_gateway_sdk::{WireDecode, WireEncode};

pub mod body;
pub mod builder;
pub mod defs;

pub use body::*;
pub use builder::*;

/// Generic CJ/T 188 frame structure.
#[derive(Debug, Clone, PartialEq)]
pub struct Cjt188Frame<T> {
    pub meter_type: MeterType,
    pub address: Cjt188Address,
    pub control: ControlWord,
    pub body: T,
}

/// A raw frame where the body is just a vector of bytes.
pub type Cjt188RawFrame = Cjt188Frame<Vec<u8>>;

/// A structured frame where the body is a parsed enum.
pub type Cjt188TypedFrame = Cjt188Frame<Cjt188Body>;

impl<T> Cjt188Frame<T> {
    pub fn new(
        meter_type: MeterType,
        address: Cjt188Address,
        control: ControlWord,
        body: T,
    ) -> Self {
        Self {
            meter_type,
            address,
            control,
            body,
        }
    }
}

/// Context for codec operations.
#[derive(Debug, Clone, Default)]
pub struct Cjt188CodecContext {
    pub function_code: Option<FunctionCode>,
    pub is_error: bool,
    pub version: Cjt188Version,
}

impl Cjt188CodecContext {
    pub fn with_function(mut self, func: FunctionCode) -> Self {
        self.function_code = Some(func);
        self
    }

    pub fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }

    pub fn with_version(mut self, version: Cjt188Version) -> Self {
        self.version = version;
        self
    }
}

impl<T> WireEncode for Cjt188Frame<T>
where
    T: WireEncode<Context = Cjt188CodecContext, Error = ProtocolError>,
{
    type Error = ProtocolError;
    type Context = Cjt188CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        // Layout: 68 + T(1) + Addr(7) + Ctrl(1) + Len(1) + Body(N) + CS(1) + 16
        // Total overhead = 1 + 1 + 7 + 1 + 1 + 1 + 1 = 13 bytes
        13 + self.body.encoded_len(ctx)
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        // 1. Header 0x68
        dst.put_u8(0x68);

        // 2. Meter type (T, 1 byte)
        dst.put_u8(u8::from(self.meter_type));

        // 3. Address (A0..A6, 7 bytes)
        dst.put_slice(&self.address.to_bytes());

        // 4. Control (1 byte)
        dst.put_u8(self.control.raw);

        // 5. Data Length & Body
        let body_len = self.body.encoded_len(ctx);
        if body_len > 255 {
            return Err(ProtocolError::Semantic(format!(
                "Body too large: {}",
                body_len
            )));
        }
        dst.put_u8(body_len as u8);

        // Render body to temp buffer to calculate checksum
        // Checksum rule: Sum of all bytes starting from frame head (0x68) to the end of Data field.
        // So: 68 + T + Addr + Ctrl + Len + Data.

        let mut sum: u32 = 0x68;

        // T
        sum += u8::from(self.meter_type) as u32;

        // Addr (A0..A6)
        for b in self.address.to_bytes() {
            sum += b as u32;
        }

        // Ctrl
        sum += self.control.raw as u32;

        // Len
        sum += body_len as u32;

        // Body
        let mut body_buf = BytesMut::with_capacity(body_len);
        self.body.encode_to(&mut body_buf, ctx)?;

        for b in body_buf.as_ref() {
            sum += *b as u32;
        }

        // Write body to dst
        dst.put_slice(&body_buf);

        // 6. Checksum
        dst.put_u8(sum as u8);

        // 7. Tail 0x16
        dst.put_u8(0x16);

        Ok(())
    }
}

impl<T> WireDecode for Cjt188Frame<T>
where
    T: WireDecode<Context = Cjt188CodecContext, Error = ProtocolError>,
{
    type Error = ProtocolError;
    type Context = Cjt188CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if input.len() < 13 {
            return Err(ProtocolError::FrameTooShort("min length 13".into()));
        }

        if input[0] != 0x68 {
            return Err(ProtocolError::InvalidFrame("missing leading 0x68".into()));
        }

        // Meter type: 1 (1 byte)
        let meter_type = MeterType::from(input[1]);

        // Address: 2..9 (7 bytes)
        let addr_bytes = &input[2..9];
        let address = Cjt188Address::from_bytes(addr_bytes)?;

        let ctrl_byte = input[9];
        let control = ControlWord::new(ctrl_byte);

        let body_ctx = if let Some(_fc) = ctx.function_code {
            // If caller provided it (e.g. from state)
            ctx.clone().with_error(control.is_error())
        } else {
            // Derive from frame control
            ctx.clone()
                .with_function(control.function_code())
                .with_error(control.is_error())
        };

        let len = input[10] as usize;
        let expected_total_len = 11 + len + 2; // Header(1) + T(1) + Addr(7) + Ctrl(1) + Len(1) + Data(len) + CS(1) + Tail(1)

        if input.len() < expected_total_len {
            return Err(ProtocolError::FrameTooShort(
                "buffer shorter than declared length".into(),
            ));
        }

        // Checksum
        let cs_calc: u8 = input[0..11 + len]
            .iter()
            .fold(0u8, |acc, &x| acc.wrapping_add(x));
        let cs_wire = input[11 + len];
        if cs_wire != cs_calc {
            return Err(ProtocolError::ChecksumMismatch {
                expected: cs_wire,
                calculated: cs_calc,
            });
        }

        if input[11 + len + 1] != 0x16 {
            return Err(ProtocolError::InvalidFrame("missing trailing 0x16".into()));
        }

        // Decode Body
        let body_data = &input[11..11 + len];
        // For zero-copy, we can pass a slice.
        // We use empty parent for now or handle it properly if needed.
        let (_, body) = T::parse(body_data, &Bytes::new(), &body_ctx)?;

        let remaining = &input[expected_total_len..];

        Ok((
            remaining,
            Self {
                meter_type,
                address,
                control,
                body,
            },
        ))
    }
}
