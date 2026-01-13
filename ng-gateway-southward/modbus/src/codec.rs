use super::types::Endianness;
use bytes::Bytes;
use ng_gateway_sdk::{DataType, DriverError, DriverResult, NGValue, NGValueCastError, ValueCodec};
use std::sync::Arc;

/// Register-based codec utilities for converting between logical values and Modbus-like register words.
///
/// This module centralizes conversions that were previously embedded in specific drivers,
/// enabling reuse across multiple industrial protocols that use 16-bit register semantics.
/// BaseCodec provides comprehensive encode/decode utilities for register-oriented protocols.
///
/// This is the single place for value conversions with awareness of endianness, scaling and
/// bounds. Higher level drivers should depend on the `Codec` facade alias which points to this
/// base implementation.
pub struct ModbusCodec;

#[allow(unused)]
impl ModbusCodec {
    #[inline(always)]
    /// Convert registers to raw bytes honoring byte and word order.
    ///
    /// For `word_order = LittleEndian`, words are reversed before byte extraction.
    pub fn words_to_bytes(
        words: &[u16],
        byte_order: Endianness,
        word_order: Endianness,
    ) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(words.len() * 2);
        if words.len() > 1 && matches!(word_order, Endianness::LittleEndian) {
            for &w in words.iter().rev() {
                let [b0, b1] = w.to_be_bytes();
                match byte_order {
                    Endianness::BigEndian => {
                        out.push(b0);
                        out.push(b1);
                    }
                    Endianness::LittleEndian => {
                        out.push(b1);
                        out.push(b0);
                    }
                }
            }
        } else {
            for &w in words.iter() {
                let [b0, b1] = w.to_be_bytes();
                match byte_order {
                    Endianness::BigEndian => {
                        out.push(b0);
                        out.push(b1);
                    }
                    Endianness::LittleEndian => {
                        out.push(b1);
                        out.push(b0);
                    }
                }
            }
        }
        out
    }

    #[inline]
    /// Parse a UTF-8/ASCII string from registers into `NGValue::String`.
    ///
    /// Trailing zero bytes are trimmed.
    pub fn parse_string_value(
        words: &[u16],
        byte_order: Endianness,
        word_order: Endianness,
    ) -> DriverResult<NGValue> {
        let mut bytes = Self::words_to_bytes(words, byte_order, word_order);
        while let Some(true) = bytes.last().map(|b| *b == 0) {
            bytes.pop();
        }
        match String::from_utf8(bytes) {
            Ok(s) => Ok(NGValue::String(Arc::<str>::from(s))),
            Err(e) => Err(DriverError::CodecError(format!(
                "Invalid UTF-8 string in registers: {e}"
            ))),
        }
    }

    #[inline]
    /// Encode a string into registers with optional fixed word length. When `target_words` is Some,
    /// the output will be padded with zeros and truncated to fit.
    pub fn encode_string_to_registers(
        s: &str,
        byte_order: Endianness,
        word_order: Endianness,
        target_words: Option<usize>,
    ) -> Vec<u16> {
        // Zero-copy friendly: build words directly from the input slice without intermediate Vec<u8>
        let mut words: Vec<u16> = Vec::with_capacity(s.len().div_ceil(2));
        let mut iter = s.as_bytes().iter().copied();
        let mut produced = 0usize;
        // Use a finite target limit only when caller specifies fixed word length
        let target_limit = target_words.unwrap_or(usize::MAX);
        while produced < target_limit {
            match (iter.next(), iter.next()) {
                (Some(a), Some(b)) => {
                    let (hi, lo) = match byte_order {
                        Endianness::BigEndian => (a, b),
                        Endianness::LittleEndian => (b, a),
                    };
                    words.push(u16::from_be_bytes([hi, lo]));
                    produced += 1;
                }
                (Some(a), None) => {
                    let (hi, lo) = match byte_order {
                        Endianness::BigEndian => (a, 0),
                        Endianness::LittleEndian => (0, a),
                    };
                    words.push(u16::from_be_bytes([hi, lo]));
                    produced += 1;
                    break;
                }
                _ => break,
            }
        }
        if words.len() > 1 && matches!(word_order, Endianness::LittleEndian) {
            words.reverse();
        }
        // Only pad when a fixed size is requested
        if let Some(tw) = target_words {
            if produced < tw {
                words.resize(tw, 0);
            }
        }
        words
    }

    #[inline]
    /// Parse raw bytes from registers into `NGValue::Binary`.
    pub fn parse_binary_value(
        words: &[u16],
        byte_order: Endianness,
        word_order: Endianness,
    ) -> NGValue {
        let bytes = Self::words_to_bytes(words, byte_order, word_order);
        NGValue::Binary(Bytes::from(bytes))
    }

    #[inline]
    /// Parse a bit_field (LSB first) from registers into an array of booleans with a fixed length.
    pub fn parse_bit_field(words: &[u16], bit_len: usize) -> Vec<bool> {
        let mut out: Vec<bool> = Vec::with_capacity(bit_len);
        let mut remaining = bit_len;
        for w in words {
            let mut n = *w as u32;
            for _ in 0..16 {
                if remaining == 0 {
                    return out;
                }
                out.push((n & 1) != 0);
                n >>= 1;
                remaining -= 1;
            }
            if remaining == 0 {
                break;
            }
        }
        // If fewer bits were available, pad false
        while out.len() < bit_len {
            out.push(false);
        }
        out
    }

    #[inline(always)]
    /// Pack a bit_field (LSB first) into registers. Output length is ceil(bits/16).
    pub fn encode_bit_field(bits: &[bool]) -> Vec<u16> {
        if bits.is_empty() {
            return Vec::new();
        }
        let words = bits.len().div_ceil(16);
        let mut out: Vec<u16> = Vec::with_capacity(words);
        let mut idx = 0;
        for _ in 0..words {
            let mut w: u16 = 0;
            for bit in 0..16 {
                if idx < bits.len() && bits[idx] {
                    w |= 1 << bit;
                }
                idx += 1;
            }
            out.push(w);
        }
        out
    }

    #[inline(always)]
    pub fn read_word(bytes: (u8, u8), byte_order: Endianness) -> (u8, u8) {
        match byte_order {
            Endianness::BigEndian => (bytes.0, bytes.1),
            Endianness::LittleEndian => (bytes.1, bytes.0),
        }
    }

    #[inline(always)]
    /// Convert a 16-bit register slice to a strongly-typed `NGValue`.
    ///
    /// This is the **hot-path** decoder that drivers should use for telemetry/attributes.
    /// It avoids building `serde_json::Value` and therefore reduces allocations.
    pub fn parse_register_value(
        words: &[u16],
        data_type: DataType,
        byte_order: Endianness,
        word_order: Endianness,
        scale: Option<f64>,
    ) -> DriverResult<NGValue> {
        let s = scale.unwrap_or(1.0);
        match data_type {
            DataType::Int16 => {
                if words.is_empty() {
                    return Err(Self::cold_err("Insufficient words for i16"));
                }
                let arr = words[0].to_be_bytes();
                let (b0, b1) = Self::read_word((arr[0], arr[1]), byte_order);
                let n = (i16::from_be_bytes([b0, b1]) as f64 * s).round() as i16;
                Ok(NGValue::Int16(n))
            }
            DataType::UInt16 => {
                if words.is_empty() {
                    return Err(Self::cold_err("Insufficient words for u16"));
                }
                let arr = words[0].to_be_bytes();
                let (b0, b1) = Self::read_word((arr[0], arr[1]), byte_order);
                let n = (u16::from_be_bytes([b0, b1]) as f64 * s).round() as u16;
                Ok(NGValue::UInt16(n))
            }
            DataType::Int32 => {
                if words.len() < 2 {
                    return Err(Self::cold_err("Insufficient words for i32"));
                }
                let (w0, w1) = if matches!(word_order, Endianness::LittleEndian) {
                    (words[1], words[0])
                } else {
                    (words[0], words[1])
                };
                let a = w0.to_be_bytes();
                let (a0, a1) = Self::read_word((a[0], a[1]), byte_order);
                let b = w1.to_be_bytes();
                let (a2, a3) = Self::read_word((b[0], b[1]), byte_order);
                let n = (i32::from_be_bytes([a0, a1, a2, a3]) as f64 * s).round() as i32;
                Ok(NGValue::Int32(n))
            }
            DataType::UInt32 => {
                if words.len() < 2 {
                    return Err(Self::cold_err("Insufficient words for u32"));
                }
                let (w0, w1) = if matches!(word_order, Endianness::LittleEndian) {
                    (words[1], words[0])
                } else {
                    (words[0], words[1])
                };
                let a = w0.to_be_bytes();
                let (a0, a1) = Self::read_word((a[0], a[1]), byte_order);
                let b = w1.to_be_bytes();
                let (a2, a3) = Self::read_word((b[0], b[1]), byte_order);
                let n = (u32::from_be_bytes([a0, a1, a2, a3]) as f64 * s).round() as u32;
                Ok(NGValue::UInt32(n))
            }
            DataType::Int64 => {
                if words.len() < 4 {
                    return Err(Self::cold_err("Insufficient words for i64"));
                }
                let (w0, w1, w2, w3) = if matches!(word_order, Endianness::LittleEndian) {
                    (words[3], words[2], words[1], words[0])
                } else {
                    (words[0], words[1], words[2], words[3])
                };
                let a = w0.to_be_bytes();
                let (a0, a1) = Self::read_word((a[0], a[1]), byte_order);
                let b = w1.to_be_bytes();
                let (a2, a3) = Self::read_word((b[0], b[1]), byte_order);
                let c = w2.to_be_bytes();
                let (a4, a5) = Self::read_word((c[0], c[1]), byte_order);
                let d = w3.to_be_bytes();
                let (a6, a7) = Self::read_word((d[0], d[1]), byte_order);
                let n = (i64::from_be_bytes([a0, a1, a2, a3, a4, a5, a6, a7]) as f64 * s).round()
                    as i64;
                Ok(NGValue::Int64(n))
            }
            DataType::UInt64 => {
                if words.len() < 4 {
                    return Err(Self::cold_err("Insufficient words for u64"));
                }
                let (w0, w1, w2, w3) = if matches!(word_order, Endianness::LittleEndian) {
                    (words[3], words[2], words[1], words[0])
                } else {
                    (words[0], words[1], words[2], words[3])
                };
                let a = w0.to_be_bytes();
                let (a0, a1) = Self::read_word((a[0], a[1]), byte_order);
                let b = w1.to_be_bytes();
                let (a2, a3) = Self::read_word((b[0], b[1]), byte_order);
                let c = w2.to_be_bytes();
                let (a4, a5) = Self::read_word((c[0], c[1]), byte_order);
                let d = w3.to_be_bytes();
                let (a6, a7) = Self::read_word((d[0], d[1]), byte_order);
                let n = (u64::from_be_bytes([a0, a1, a2, a3, a4, a5, a6, a7]) as f64 * s).round()
                    as u64;
                Ok(NGValue::UInt64(n))
            }
            DataType::Float32 => {
                if words.len() < 2 {
                    return Err(Self::cold_err("Insufficient words for f32"));
                }
                let (w0, w1) = if matches!(word_order, Endianness::LittleEndian) {
                    (words[1], words[0])
                } else {
                    (words[0], words[1])
                };
                let a = w0.to_be_bytes();
                let (a0, a1) = Self::read_word((a[0], a[1]), byte_order);
                let b = w1.to_be_bytes();
                let (a2, a3) = Self::read_word((b[0], b[1]), byte_order);
                let n = f32::from_be_bytes([a0, a1, a2, a3]) as f64 * s;
                Ok(NGValue::Float32(n as f32))
            }
            DataType::Float64 => {
                if words.len() < 4 {
                    return Err(Self::cold_err("Insufficient words for f64"));
                }
                let (w0, w1, w2, w3) = if matches!(word_order, Endianness::LittleEndian) {
                    (words[3], words[2], words[1], words[0])
                } else {
                    (words[0], words[1], words[2], words[3])
                };
                let a = w0.to_be_bytes();
                let (a0, a1) = Self::read_word((a[0], a[1]), byte_order);
                let b = w1.to_be_bytes();
                let (a2, a3) = Self::read_word((b[0], b[1]), byte_order);
                let c = w2.to_be_bytes();
                let (a4, a5) = Self::read_word((c[0], c[1]), byte_order);
                let d = w3.to_be_bytes();
                let (a6, a7) = Self::read_word((d[0], d[1]), byte_order);
                let n = f64::from_be_bytes([a0, a1, a2, a3, a4, a5, a6, a7]) * s;
                Ok(NGValue::Float64(n))
            }
            DataType::Boolean => {
                let b = !words.is_empty() && words[0] != 0;
                Ok(NGValue::Boolean(b))
            }
            DataType::String => Self::parse_string_value(words, byte_order, word_order),
            DataType::Binary => Ok(Self::parse_binary_value(words, byte_order, word_order)),
            DataType::Timestamp => {
                if words.len() < 4 {
                    return Err(Self::cold_err("Insufficient words for timestamp (i64 ms)"));
                }
                let (w0, w1, w2, w3) = if matches!(word_order, Endianness::LittleEndian) {
                    (words[3], words[2], words[1], words[0])
                } else {
                    (words[0], words[1], words[2], words[3])
                };
                let a = w0.to_be_bytes();
                let (a0, a1) = Self::read_word((a[0], a[1]), byte_order);
                let b = w1.to_be_bytes();
                let (a2, a3) = Self::read_word((b[0], b[1]), byte_order);
                let c = w2.to_be_bytes();
                let (a4, a5) = Self::read_word((c[0], c[1]), byte_order);
                let d = w3.to_be_bytes();
                let (a6, a7) = Self::read_word((d[0], d[1]), byte_order);
                let ms = i64::from_be_bytes([a0, a1, a2, a3, a4, a5, a6, a7]);
                Ok(NGValue::Timestamp(ms))
            }
            DataType::Int8 | DataType::UInt8 => {
                if words.is_empty() {
                    return Err(Self::cold_err("Insufficient words for u8/i8"));
                }
                let raw = words[0];
                let arr = raw.to_be_bytes();
                let (b0, b1) = Self::read_word((arr[0], arr[1]), byte_order);
                let u = u16::from_be_bytes([b0, b1]) as f64 * s;
                if matches!(data_type, DataType::UInt8) {
                    return ValueCodec::coerce_f64_to_value(u, DataType::UInt8, None)
                        .ok_or(Self::cold_err("u8 out of range"));
                }
                // Int8
                let i = (i16::from_be_bytes([b0, b1]) as f64 * s).round();
                ValueCodec::coerce_f64_to_value(i, DataType::Int8, None)
                    .ok_or(Self::cold_err("i8 out of range"))
            }
        }
    }

    /// Encode coils from a strongly-typed `NGValue`.
    ///
    /// # Best practice
    /// `Driver::write_point` receives `NGValue` and should avoid round-tripping through JSON.
    /// Keep `encode_coils(&serde_json::Value, ..)` for `execute_action` which may accept objects/arrays.
    pub fn encode_coils(value: &NGValue, target_len: Option<usize>) -> DriverResult<Vec<bool>> {
        let b = bool::try_from(value).map_err(|e: NGValueCastError| {
            DriverError::ValidationError(format!(
                "Expected boolean value, got {:?}: {e}",
                value.data_type()
            ))
        })?;
        let mut out = match target_len {
            Some(n) => vec![b; n],
            None => vec![b],
        };
        if let Some(n) = target_len {
            if out.len() > n {
                out.truncate(n);
            } else if out.len() < n {
                out.resize(n, b);
            }
        }
        Ok(out)
    }

    /// Encode registers from a strongly-typed `NGValue`.
    ///
    /// This is the `NGValue` counterpart of `encode_registers_from_value_or_array` (scalar path).
    pub fn encode_registers_from_value(
        value: &NGValue,
        data_type: DataType,
        byte_order: Endianness,
        word_order: Endianness,
    ) -> DriverResult<Vec<u16>> {
        match data_type {
            DataType::Int8 | DataType::Int16 => {
                let v = i16::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected int16 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::UInt8 | DataType::UInt16 => {
                let v = u16::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected uint16 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::Int32 => {
                let v = i32::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected int32 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::UInt32 => {
                let v = u32::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected uint32 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::Int64 => {
                let v = i64::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected int64 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::UInt64 => {
                let v = u64::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected uint64 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::Float32 => {
                let v = f32::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected float32 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::Float64 => {
                let v = f64::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected float64 value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(Self::bytes_to_words(
                    v.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::Boolean => {
                let b = bool::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected boolean value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                let n: u16 = if b { 1 } else { 0 };
                Ok(Self::bytes_to_words(
                    n.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                ))
            }
            DataType::String => match value {
                NGValue::String(s) => Ok(Self::encode_string_to_registers(
                    s.as_ref(),
                    byte_order,
                    word_order,
                    None,
                )),
                _ => Err(DriverError::CodecError("Expected String".to_string())),
            },
            DataType::Binary => match value {
                NGValue::Binary(b) => Ok(Self::bytes_to_words_from_slice(
                    b.as_ref(),
                    byte_order,
                    word_order,
                )),
                _ => Err(DriverError::CodecError("Expected Binary".to_string())),
            },
            DataType::Timestamp => match value {
                NGValue::Timestamp(ms) => Ok(Self::bytes_to_words(
                    ms.to_be_bytes().to_vec(),
                    byte_order,
                    word_order,
                )),
                _ => Err(DriverError::CodecError("Expected Timestamp".to_string())),
            },
        }
    }

    #[inline(always)]
    pub fn bytes_to_words(
        bytes: Vec<u8>,
        byte_order: Endianness,
        word_order: Endianness,
    ) -> Vec<u16> {
        // Delegate to slice-based version to avoid extra copies or swaps
        Self::bytes_to_words_from_slice(&bytes, byte_order, word_order)
    }

    #[inline]
    /// Convert a byte slice into register words without copying the input buffer.
    /// If the number of bytes is odd, the last word is padded with zero.
    pub fn bytes_to_words_from_slice(
        bytes: &[u8],
        byte_order: Endianness,
        word_order: Endianness,
    ) -> Vec<u16> {
        let even_len = bytes.len() & !1;
        let has_tail = (bytes.len() & 1) == 1;
        let mut words: Vec<u16> = Vec::with_capacity(bytes.len().div_ceil(2));
        let mut i = 0usize;
        while i < even_len {
            let (b0, b1) = match byte_order {
                Endianness::BigEndian => (bytes[i], bytes[i + 1]),
                Endianness::LittleEndian => (bytes[i + 1], bytes[i]),
            };
            words.push(u16::from_be_bytes([b0, b1]));
            i += 2;
        }
        if has_tail {
            let last = bytes[bytes.len() - 1];
            let (b0, b1) = match byte_order {
                Endianness::BigEndian => (last, 0),
                Endianness::LittleEndian => (0, last),
            };
            words.push(u16::from_be_bytes([b0, b1]));
        }
        if words.len() > 1 && matches!(word_order, Endianness::LittleEndian) {
            words.reverse();
        }
        words
    }

    #[inline(never)]
    #[cold]
    fn cold_err(msg: &str) -> DriverError {
        DriverError::CodecError(msg.to_string())
    }

    #[inline(never)]
    #[cold]
    fn cold_err_string(msg: String) -> DriverError {
        DriverError::CodecError(msg)
    }
}

/// Zero-copy friendly bytes codec for converting between raw bytes and register words.
///
/// This codec complements `BaseCodec` by exposing APIs that take/return `bytes::Bytes` to
/// integrate with network buffers or file-mapped regions with minimal copies.
#[allow(unused)]
pub struct BytesCodec;

#[allow(unused)]
impl BytesCodec {
    #[inline]
    /// Convert register words to a `Bytes` buffer according to byte/word order.
    pub fn words_to_bytes(
        words: &[u16],
        byte_order: Endianness,
        word_order: Endianness,
    ) -> bytes::Bytes {
        let v = ModbusCodec::words_to_bytes(words, byte_order, word_order);
        bytes::Bytes::from(v)
    }

    #[inline]
    /// Convert a byte slice into register words according to byte/word order.
    /// If the number of bytes is odd, a trailing zero byte is assumed.
    pub fn bytes_to_words<B: AsRef<[u8]>>(
        bytes: B,
        byte_order: Endianness,
        word_order: Endianness,
    ) -> Vec<u16> {
        ModbusCodec::bytes_to_words_from_slice(bytes.as_ref(), byte_order, word_order)
    }
}
