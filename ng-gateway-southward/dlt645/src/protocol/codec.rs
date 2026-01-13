use crate::protocol::{
    error::ProtocolError,
    frame::{Dl645CodecContext, Dl645TypedFrame},
};
use crate::types::Dl645Version;
use bytes::{Buf, Bytes, BytesMut};
use ng_gateway_sdk::{DriverError, DriverResult, WireDecode, WireEncode};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

/// Try to extract a complete DL/T 645 frame from the provided buffer.
///
/// This helper scans the buffer for the DL/T 645 frame signature, validates
/// the frame structure, and fully parses it into a typed frame.
pub fn try_extract_frame(
    buf: &[u8],
    version: Dl645Version,
) -> Result<Option<(Dl645TypedFrame, usize)>, ProtocolError> {
    if buf.len() < 12 {
        return Ok(None);
    }

    // Scan for 0x68
    let mut i = 0usize;
    while i + 12 <= buf.len() {
        if buf[i] != 0x68 {
            i += 1;
            continue;
        }
        // Need at least header + min body + tail
        if i + 12 > buf.len() {
            // Not enough data yet to even check length byte
            // But we might have a potential start.
            // Safest is to break and wait for more data if we are at end.
            break;
        }

        // Second 0x68 at offset 7
        if buf[i + 7] != 0x68 {
            i += 1;
            continue;
        }

        let len = buf[i + 9] as usize;
        let frame_len = 10 + len + 2; // 68...68(control)(len)(data...)(cs)(16)

        if i + frame_len > buf.len() {
            // Partial frame, wait for more data.
            break;
        }

        // Try parsing
        let slice = &buf[i..i + frame_len];
        let ctx = Dl645CodecContext::new(version);

        // Since WireDecode::parse expects the slice to match exactly or return remaining,
        // and we already sliced the buffer to the exact frame length,
        // remaining should be empty relative to `slice`.
        // However, `parse` doesn't enforce check of leading bytes if we don't implement it carefully.
        // Dl645Frame::parse checks 0x68 etc.

        // Note: Dl645Frame::parse takes `input`, `parent`, `ctx`.
        // `parent` is unused by our implementation for now, so we pass empty Bytes.
        let empty_parent = Bytes::new();

        match Dl645TypedFrame::parse(slice, &empty_parent, &ctx) {
            Ok((_, frame)) => {
                return Ok(Some((frame, i + frame_len)));
            }
            Err(_) => {
                // Parsing failed (e.g. checksum), so this is not a valid frame (or corrupted).
                // Skip this 0x68 and continue scanning.
                i += 1;
                continue;
            }
        }
    }
    Ok(None)
}

/// Decode a DL/T 645 BCD-encoded number into `f64` with the given decimals.
pub fn decode_bcd_to_f64(bytes: &[u8], decimals: u8, is_signed: bool) -> DriverResult<f64> {
    if bytes.is_empty() {
        return Err(DriverError::CodecError(
            "BCD payload must not be empty".to_string(),
        ));
    }

    let len = bytes.len();
    if len > 8 {
        return Err(DriverError::CodecError(
            "BCD payload too long for u64".to_string(),
        ));
    }

    let mut value: u64 = 0;
    let mut multiplier: u64 = 1;

    // Process all bytes except the last one (which contains sign info)
    for &b in &bytes[..len - 1] {
        let lo = (b & 0x0F) as u64;
        let hi = ((b >> 4) & 0x0F) as u64;

        if lo > 9 || hi > 9 {
            return Err(DriverError::CodecError("Invalid BCD digit".to_string()));
        }
        value += (lo + hi * 10) * multiplier;
        multiplier *= 100;
    }

    // Process the last byte (MSB)
    let last_byte = bytes[len - 1];
    let lo = (last_byte & 0x0F) as u64;
    let mut hi_byte = (last_byte >> 4) & 0x0F;
    let mut negative = false;

    if is_signed {
        // Check sign bit (bit 3 of high nibble)
        if (hi_byte & 0x08) != 0 {
            negative = true;
            hi_byte &= 0x07;
        }
    }

    if lo > 9 || hi_byte > 9 {
        return Err(DriverError::CodecError("Invalid BCD digit".to_string()));
    }

    let hi = hi_byte as u64;
    value += (lo + hi * 10) * multiplier;

    let scale = 10f64.powi(decimals as i32);
    let float_val = value as f64;
    let signed = if negative { -float_val } else { float_val };

    Ok(signed / scale)
}

/// Encode an `f64` value into BCD representation with the given decimals.
pub fn encode_f64_to_bcd(value: f64, decimals: u8, is_signed: bool) -> DriverResult<Vec<u8>> {
    let negative = value < 0.0;
    if negative && !is_signed {
        return Err(DriverError::CodecError(format!(
            "Cannot encode negative value {} for unsigned type",
            value
        )));
    }

    let scale = 10f64.powi(decimals as i32);
    let scaled = (value.abs() * scale).round();
    let mut int_val = scaled as u64;

    let mut bytes = Vec::new();

    if int_val == 0 {
        bytes.push(0);
    } else {
        while int_val > 0 {
            let lo = (int_val % 10) as u8;
            int_val /= 10;
            let hi = (int_val % 10) as u8;
            int_val /= 10;
            bytes.push((hi << 4) | lo);
        }
    }

    if is_signed {
        // Safe unwrap because we always push at least one byte
        let last = bytes.last_mut().unwrap();
        if negative {
            *last |= 0x80;
        } else if (*last & 0x80) != 0 {
            return Err(DriverError::CodecError(format!(
                "Value {} too large for signed BCD representation",
                value
            )));
        }
    }

    Ok(bytes)
}

/// DL/T 645 framed codec for use with `tokio_util::codec::Framed`.
#[derive(Debug, Clone)]
pub struct Dl645FrameCodec {
    /// Protocol version for encoding context.
    pub version: Dl645Version,
}

impl Dl645FrameCodec {
    /// Create a new codec with the given maximum frame size and version.
    pub fn new(version: Dl645Version) -> Self {
        Self { version }
    }

    // Helper to extract point value from a response frame.
    // This was previously part of Dl645Codec impl but might be used here or in driver.
    // Actually, driver calls `Dl645Codec::decode_point_value`.
    // We should ensure that `Dl645Codec` (if it exists separate from this file) supports Dl645TypedFrame.
    // This file seems to be `protocol/codec.rs`.
    // Is there another `codec.rs`?
    // Yes, `ng-gateway-southward/dlt645/src/codec.rs` exists.
}

impl Decoder for Dl645FrameCodec {
    type Item = Dl645TypedFrame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.is_empty() {
            return Ok(None);
        }

        let buf: &[u8] = src;
        // Pass version to extractor
        let extracted = match try_extract_frame(buf, self.version) {
            Ok(opt) => opt,
            Err(e) => {
                src.clear();
                return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
            }
        };

        let (owned, consumed) = match extracted {
            Some(v) => v,
            None => return Ok(None),
        };

        src.advance(consumed);

        Ok(Some(owned))
    }
}

impl Encoder<Dl645TypedFrame> for Dl645FrameCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Dl645TypedFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let ctx = Dl645CodecContext {
            version: self.version,
            function_code: None,
            is_exception: false,
        };

        // WireEncode::encode_to
        match item.encode_to(dst, &ctx) {
            Ok(_) => Ok(()),
            Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        }
    }
}
