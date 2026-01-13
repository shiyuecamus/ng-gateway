use crate::protocol::error::{Dl645ExceptionCode, ProtocolError};
use bytes::{BufMut, Bytes, BytesMut};
use ng_gateway_sdk::{WireDecode, WireEncode};

pub mod body;
pub mod builder;
pub use body::*;
pub use builder::*;

pub mod defs;
pub use defs::*;

/// Generic DL/T 645 frame structure.
///
/// This structure represents a full DL/T 645 frame, parameterized by the body type.
/// It can hold raw bytes (`Vec<u8>`) or a structured body (`Dl645Body`).
#[derive(Debug, Clone, PartialEq)]
pub struct Dl645Frame<T> {
    /// 6-byte BCD address in little-endian order.
    pub address: Dl645Address,
    /// Control word (function code, direction, flags).
    pub control: Dl645ControlWord,
    /// Frame payload.
    pub body: T,
}

/// A raw frame where the body is just a vector of bytes (cleaned, without 0x33 offset).
pub type Dl645RawFrame = Dl645Frame<Vec<u8>>;

/// A structured frame where the body is a parsed enum.
pub type Dl645TypedFrame = Dl645Frame<Dl645Body>;

impl<T> Dl645Frame<T> {
    /// Create a new frame.
    pub fn new(address: Dl645Address, control: Dl645ControlWord, body: T) -> Self {
        Self {
            address,
            control,
            body,
        }
    }
}

impl Dl645TypedFrame {
    /// Create a new request frame with automatically derived control word.
    pub fn new_request(
        address: Dl645Address,
        body: Dl645Body,
        version: crate::types::Dl645Version,
    ) -> Self {
        let control = Dl645ControlWord::for_request(version, body.function_code());
        Self::new(address, control, body)
    }
}

impl<T> WireEncode for Dl645Frame<T>
where
    T: WireEncode<Context = Dl645CodecContext, Error = ProtocolError>,
{
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn encoded_len(&self, ctx: &Self::Context) -> usize {
        // Frame layout: 68 + addr(6) + 68 + ctrl(1) + len(1) + data(N) + cs(1) + 16
        // Total overhead = 1 + 6 + 1 + 1 + 1 + 1 + 1 = 12 bytes
        12 + self.body.encoded_len(ctx)
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, ctx: &Self::Context) -> Result<(), Self::Error> {
        // 1. Header 0x68
        dst.put_u8(0x68);

        // 2. Address
        dst.put_slice(&self.address.0);

        // 3. Header 0x68
        dst.put_u8(0x68);

        // 4. Control
        dst.put_u8(self.control.raw);

        // 5. Data Length & Body
        // We need to encode the body first to know its length and to apply +0x33
        // Since we need to modify the bytes (add 0x33) and calculate checksum,
        // we render the body to a temporary buffer.
        let body_len = self.body.encoded_len(ctx);
        if body_len > 255 {
            return Err(ProtocolError::FrameTooLarge(body_len));
        }
        dst.put_u8(body_len as u8);

        // Temporary buffer for body
        let mut body_buf = BytesMut::with_capacity(body_len);
        self.body.encode_to(&mut body_buf, ctx)?;

        // Apply 0x33 offset and write to dst
        for b in body_buf.as_ref() {
            dst.put_u8(b.wrapping_add(0x33));
        }

        // 6. Checksum
        // CS is sum of [0x68 .. last data byte]
        // But wait, the CS includes the FIRST 0x68?
        // Spec: "CS is the sum modulo 256 of all bytes from the frame start character (68H) to the data field."
        // Let's verify existing logic:
        // "Checksum from first 0x68 to last data byte."
        // We can't easily access the bytes we just wrote to `dst` if `B` is a generic BufMut.
        // So we might need to calculate it as we go or write to a full temporary buffer.
        // For performance, calculating as we go is better, but `dst` might not support reading back.
        // Let's write everything to a `BytesMut` first? Or calculate sum manually.

        // Let's verify what `dst` allows. `BufMut` is write-only.
        // However, we know what we wrote.
        let mut sum: u32 = 0x68;
        for b in self.address.0 {
            sum += b as u32;
        }
        sum += 0x68;
        sum += self.control.raw as u32;
        sum += body_len as u32;
        for b in body_buf.as_ref() {
            sum += b.wrapping_add(0x33) as u32;
        }

        dst.put_u8(sum as u8);

        // 7. Tail 0x16
        dst.put_u8(0x16);

        Ok(())
    }
}

impl<T> WireDecode for Dl645Frame<T>
where
    T: WireDecode<Context = Dl645CodecContext, Error = ProtocolError>,
{
    type Error = ProtocolError;
    type Context = Dl645CodecContext;

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self), Self::Error> {
        if input.len() < 12 {
            return Err(ProtocolError::InvalidFrame("frame too short".to_string()));
        }
        if input[0] != 0x68 {
            return Err(ProtocolError::InvalidFrame(
                "missing leading 0x68".to_string(),
            ));
        }

        // Address is at 1..7
        let mut address_bytes = [0u8; 6];
        address_bytes.copy_from_slice(&input[1..7]);
        let address = Dl645Address(address_bytes);

        if input[7] != 0x68 {
            return Err(ProtocolError::InvalidFrame(
                "missing second 0x68".to_string(),
            ));
        }

        let ctrl_byte = input[8];
        let control = Dl645ControlWord::new(ctrl_byte);
        let len = input[9] as usize;

        let expected_total_len = 10 + len + 2; // Header(10) + Data(len) + CS(1) + Tail(1)
        if input.len() < expected_total_len {
            return Err(ProtocolError::InvalidFrame(
                "buffer shorter than declared length".to_string(),
            ));
        }

        // Verify Checksum
        let cs_calc: u8 = input[0..10 + len]
            .iter()
            .fold(0u8, |acc, &v| acc.wrapping_add(v));

        let cs_wire = input[10 + len];
        if cs_wire != cs_calc {
            return Err(ProtocolError::ChecksumMismatch);
        }

        if input[10 + len + 1] != 0x16 {
            return Err(ProtocolError::InvalidFrame(
                "missing trailing 0x16".to_string(),
            ));
        }

        // Extract and clean data
        let raw_data = &input[10..10 + len];
        let mut cleaned_buf = BytesMut::with_capacity(len);
        for b in raw_data {
            cleaned_buf.put_u8(b.wrapping_sub(0x33));
        }
        let cleaned_bytes = cleaned_buf.freeze();

        // Decode Body
        // We need to pass the function code to the body context if it wasn't already there.
        // The frame's control word tells us the function code.
        let function = control.function();
        let is_exception = control.is_exception_response();
        let body_ctx = ctx
            .clone()
            .with_function(function)
            .with_exception(is_exception);

        // Zero-copy optimization:
        // We pass the cleaned_bytes as both input (slice) and parent (owner).
        // The Body parser can then slice_ref from parent to hold data without copying.
        let (_, body) = T::parse(&cleaned_bytes, &cleaned_bytes, &body_ctx)?;

        let remaining = &input[expected_total_len..];

        Ok((
            remaining,
            Dl645Frame {
                address,
                control,
                body,
            },
        ))
    }
}

/// Borrowed DL/T 645 frame view over an underlying buffer.
/// Kept for compatibility or low-level inspection.
pub struct Dl645FrameRef<'a> {
    pub address: Dl645Address,
    pub control: Dl645ControlWord,
    pub data: &'a [u8],
}

impl<'a> Dl645FrameRef<'a> {
    /// Decode a DL/T 645 frame from the provided buffer.
    /// Returns the raw frame with data still having 0x33 offset.
    pub fn decode(buf: &'a [u8]) -> Result<Dl645FrameRef<'a>, ProtocolError> {
        if buf.len() < 12 {
            return Err(ProtocolError::InvalidFrame("frame too short".to_string()));
        }
        if buf[0] != 0x68 || buf[7] != 0x68 {
            return Err(ProtocolError::InvalidFrame("missing 0x68".to_string()));
        }

        let len = buf[9] as usize;
        let expected_len = 12 + len;
        if buf.len() < expected_len {
            return Err(ProtocolError::InvalidFrame("buffer too short".to_string()));
        }

        // Checksum
        let cs_calc: u8 = buf[0..10 + len]
            .iter()
            .fold(0, |acc, &x| acc.wrapping_add(x));
        if buf[10 + len] != cs_calc {
            return Err(ProtocolError::ChecksumMismatch);
        }

        if buf[11 + len] != 0x16 {
            return Err(ProtocolError::InvalidFrame("missing 0x16".to_string()));
        }

        let mut address_bytes = [0u8; 6];
        address_bytes.copy_from_slice(&buf[1..7]);
        let address = Dl645Address(address_bytes);
        let control = Dl645ControlWord::new(buf[8]);
        let data = &buf[10..10 + len];

        Ok(Dl645FrameRef {
            address,
            control,
            data,
        })
    }
}

/// High-level semantic view over a single DL/T645 response frame.
#[derive(Debug)]
pub struct Dl645ResponseFrameView<'a> {
    pub frame: &'a Dl645TypedFrame,
    pub ctrl: Dl645ControlWord,
    pub function: Dl645Function,
    pub is_exception: bool,
    pub exception_code: Option<Dl645ExceptionCode>,
    pub has_following_frame: bool,
}

impl Dl645TypedFrame {
    /// Build a semantic response view for this frame using DL/T645 rules.
    pub fn as_response_view(&self) -> Dl645ResponseFrameView<'_> {
        let word = self.control;
        let function = word.function();
        let is_exception = word.is_exception_response();
        let has_following_frame = word.has_following_frame();

        let exception_code = if is_exception {
            // For exception frames, body usually contains exception code.
            // We use data_payload() to inspect the raw content.
            self.body
                .data_payload()
                .and_then(|d| d.first().map(|&b| Dl645ExceptionCode::from_byte(b)))
        } else {
            None
        };

        Dl645ResponseFrameView {
            frame: self,
            ctrl: word,
            function,
            is_exception,
            exception_code,
            has_following_frame,
        }
    }
}

/// Encode a human-readable meter address string into a 6-byte BCD array.
pub fn encode_address_from_str(addr: &str) -> Result<Dl645Address, ProtocolError> {
    if addr.len() != 12 || !addr.chars().all(|c| c.is_ascii_digit()) {
        return Err(ProtocolError::InvalidFrame(
            "address must be 12 decimal digits".to_string(),
        ));
    }
    let mut bytes = [0u8; 6];
    for i in 0..6 {
        let hi = addr.as_bytes()[2 * i] - b'0';
        let lo = addr.as_bytes()[2 * i + 1] - b'0';
        bytes[5 - i] = (hi << 4) | lo;
    }
    Ok(Dl645Address(bytes))
}

/// Decode a 6-byte BCD meter address into a human-readable string.
pub fn decode_address_to_str(addr: Dl645Address) -> String {
    let mut out = String::with_capacity(12);
    for b in addr.0.iter().rev() {
        let hi = (b >> 4) & 0x0F;
        let lo = b & 0x0F;
        out.push(char::from(b'0' + hi));
        out.push(char::from(b'0' + lo));
    }
    out
}

/// Parse a DI string into a `u32`.
pub fn parse_di_str(di: &str) -> Option<u32> {
    if !(di.len() == 4 || di.len() == 8) {
        return None;
    }

    if !di.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    if di.len() == 4 {
        u32::from_str_radix(di, 16).ok().map(|v| v & 0xFFFF)
    } else {
        u32::from_str_radix(di, 16).ok()
    }
}
