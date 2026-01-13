use crate::{
    protocol::{
        error::ProtocolError,
        frame::{Cjt188CodecContext, Cjt188TypedFrame},
    },
    types::Cjt188Version,
};
use bytes::{Buf, Bytes, BytesMut};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use ng_gateway_sdk::{WireDecode, WireEncode};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

/// Decode a BCD-encoded buffer into f64.
///
/// `decimals` indicates implicit decimal places.
/// e.g. BCD "1234", decimals 2 -> 12.34
pub fn decode_bcd_to_f64(
    bytes: &[u8],
    decimals: u8,
    _is_signed: bool,
) -> Result<f64, ProtocolError> {
    if bytes.is_empty() {
        return Err(ProtocolError::Semantic("BCD payload empty".into()));
    }

    let len = bytes.len();
    if len > 8 {
        return Err(ProtocolError::Semantic("BCD payload too long".into()));
    }

    let mut value: u64 = 0;
    let mut multiplier: u64 = 1;

    // CJ/T 188 usually uses Little Endian BCD.
    // e.g. [0x34, 0x12] -> 1234
    // bytes[0] is lowest significant digits.
    for &b in bytes.iter() {
        let lo = (b & 0x0F) as u64;
        let hi = ((b >> 4) & 0x0F) as u64;

        if lo > 9 || hi > 9 {
            return Err(ProtocolError::Semantic(format!(
                "Invalid BCD byte: {:02X}",
                b
            )));
        }

        value += (lo + hi * 10) * multiplier;
        multiplier *= 100;
    }

    let scale = 10f64.powi(decimals as i32);
    Ok(value as f64 / scale)
}

pub fn encode_f64_to_bcd(value: f64, len: usize, decimals: u8) -> Result<Vec<u8>, ProtocolError> {
    let scale = 10f64.powi(decimals as i32);
    let scaled = (value.abs() * scale).round();
    let mut int_val = scaled as u64;

    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        let lo = (int_val % 10) as u8;
        int_val /= 10;
        let hi = (int_val % 10) as u8;
        int_val /= 10;
        bytes.push((hi << 4) | lo);
    }

    Ok(bytes)
}

/// Decode single BCD byte to u8 (0-99)
///
/// # Arguments
/// * `byte` - BCD encoded byte (e.g., 0x25 = 25)
///
/// # Returns
/// Decimal value (0-99) or error if invalid BCD
pub fn decode_bcd_byte(byte: u8) -> Result<u8, ProtocolError> {
    let high = (byte >> 4) & 0x0F;
    let low = byte & 0x0F;

    if high > 9 || low > 9 {
        return Err(ProtocolError::Semantic(format!(
            "Invalid BCD byte: 0x{:02X}",
            byte
        )));
    }

    Ok(high * 10 + low)
}

/// Decode BCD-encoded integer value
///
/// # Arguments
/// * `bytes` - BCD encoded bytes (little-endian)
///
/// # Returns
/// Integer value as u64
pub fn decode_bcd_integer(bytes: &[u8]) -> Result<u64, ProtocolError> {
    if bytes.is_empty() {
        return Err(ProtocolError::Semantic("BCD payload empty".into()));
    }

    let mut value: u64 = 0;
    let mut multiplier: u64 = 1;

    // Little-endian BCD: lowest byte first
    for &byte in bytes {
        let low = (byte & 0x0F) as u64;
        let high = ((byte >> 4) & 0x0F) as u64;

        if low > 9 || high > 9 {
            return Err(ProtocolError::Semantic(format!(
                "Invalid BCD byte: 0x{:02X}",
                byte
            )));
        }

        value += (low + high * 10) * multiplier;
        multiplier *= 100;
    }

    Ok(value)
}

/// Decode CJ/T 188 date-time field (7 bytes BCD).
///
/// # Format
/// CJ/T 188 uses BCD-encoded time fields, and on the wire it is typically
/// **little-endian in field order**:
///
/// - `SS` (1 byte): second (00-59)
/// - `mm` (1 byte): minute (00-59)
/// - `hh` (1 byte): hour (00-23)
/// - `DD` (1 byte): day (01-31)
/// - `MM` (1 byte): month (01-12)
/// - `YY` (1 byte): year within century (00-99)
/// - `CC` (1 byte): century (e.g. 0x20 -> 20)
///
/// Example: `55 14 09 09 01 26 20` -> `2026-01-09 09:14:55`.
///
/// # Arguments
/// * `bytes` - Exactly 7 bytes of BCD-encoded datetime
///
/// # Returns
/// NaiveDateTime on success
///
pub fn decode_datetime(bytes: &[u8]) -> Result<NaiveDateTime, ProtocolError> {
    if bytes.len() != 7 {
        return Err(ProtocolError::Semantic(format!(
            "DateTime field must be 7 bytes (SS mm hh DD MM YY CC), got {} bytes",
            bytes.len()
        )));
    }

    // CJ/T 188 little-endian field order: SS mm hh DD MM YY CC
    let second = decode_bcd_byte(bytes[0])? as u32;
    let minute = decode_bcd_byte(bytes[1])? as u32;
    let hour = decode_bcd_byte(bytes[2])? as u32;
    let day = decode_bcd_byte(bytes[3])? as u32;
    let month = decode_bcd_byte(bytes[4])? as u32;

    let yy = decode_bcd_byte(bytes[5])? as i32;
    let cc = decode_bcd_byte(bytes[6])? as i32;
    let year = cc * 100 + yy;

    build_datetime(year, month, day, hour, minute, second)
}

/// Decode binary unsigned integer (little-endian)
///
/// # Arguments
/// * `bytes` - Binary encoded bytes (1, 2, or 4 bytes)
///
/// # Returns
/// Unsigned integer value as u64
///
/// # Examples
/// ```
/// # use ng_driver_cjt188::protocol::codec::decode_binary;
/// let bytes = &[0x1F, 0x90]; // Little-endian: 0x901F
/// assert_eq!(decode_binary(bytes).unwrap(), 0x901F);
/// ```
pub fn decode_binary(bytes: &[u8]) -> Result<u64, ProtocolError> {
    match bytes.len() {
        1 => Ok(bytes[0] as u64),
        2 => Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as u64),
        4 => Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64),
        len => Err(ProtocolError::Semantic(format!(
            "Unsupported binary field length: {} bytes (expected 1, 2, or 4)",
            len
        ))),
    }
}

/// Build `NaiveDateTime` with validation and rich errors.
#[inline]
fn build_datetime(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Result<NaiveDateTime, ProtocolError> {
    let date = NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| {
        ProtocolError::Semantic(format!("Invalid date: year={year} month={month} day={day}"))
    })?;
    let time = NaiveTime::from_hms_opt(hour, minute, second).ok_or_else(|| {
        ProtocolError::Semantic(format!(
            "Invalid time: hour={hour} minute={minute} second={second}"
        ))
    })?;
    Ok(NaiveDateTime::new(date, time))
}

#[derive(Debug, Clone)]
pub struct Cjt188FrameCodec {
    version: Cjt188Version,
}

impl Cjt188FrameCodec {
    pub fn new(version: Cjt188Version) -> Self {
        Self { version }
    }
}

impl Default for Cjt188FrameCodec {
    fn default() -> Self {
        Self::new(Cjt188Version::default())
    }
}

impl Decoder for Cjt188FrameCodec {
    type Item = Cjt188TypedFrame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.is_empty() {
            return Ok(None);
        }

        let mut i = 0;
        while i < src.len() {
            if src[i] != 0x68 {
                i += 1;
                continue;
            }

            if i + 13 > src.len() {
                if i > 0 {
                    src.advance(i);
                }
                return Ok(None);
            }

            let len_byte = src[i + 10];
            let body_len = len_byte as usize;
            let total_len = 11 + body_len + 2; // 11 header (68+T+Addr+Ctrl+Len) + body + CS + Tail

            if i + total_len > src.len() {
                if i > 0 {
                    src.advance(i);
                }
                return Ok(None);
            }

            if src[i + total_len - 1] != 0x16 {
                i += 1;
                continue;
            }

            let cs_expected = src[i + 11 + body_len];
            let cs_calc = src[i..i + 11 + body_len]
                .iter()
                .fold(0u8, |acc, &x| acc.wrapping_add(x));

            if cs_expected != cs_calc {
                i += 1;
                continue;
            }

            src.advance(i);
            let frame_bytes = src.split_to(total_len);
            let ctx = Cjt188CodecContext::default().with_version(self.version);

            let input = &frame_bytes[..];
            match Cjt188TypedFrame::parse(input, &Bytes::new(), &ctx) {
                Ok((_, frame)) => return Ok(Some(frame)),
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
            }
        }

        src.clear();
        Ok(None)
    }
}

impl Encoder<Cjt188TypedFrame> for Cjt188FrameCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Cjt188TypedFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let ctx = Cjt188CodecContext::default().with_version(self.version);
        item.encode_to(dst, &ctx)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }
}
