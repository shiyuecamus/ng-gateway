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
    /// Convert a 16-bit register slice to a strongly-typed `NGValue`.
    ///
    /// # Process Pipeline
    /// 1. **Normalize**: Apply Word-Swap and Byte-Swap to canonicalize registers into a Big-Endian byte stream.
    /// 2. **Decode**: Read the raw value based on the available byte length.
    ///    - If length matches strict type requirements (e.g. 4 bytes for Float32), read directly.
    ///    - If length is smaller (e.g. 2 bytes for Float32), read as Integer and cast (Smart Cast).
    /// 3. **Scale**: Apply linear scaling (ax + b).
    /// 4. **Box**: Wrap into the requested `NGValue` variant.
    pub fn parse_register_value(
        words: &[u16],
        data_type: DataType,
        byte_order: Endianness,
        word_order: Endianness,
        scale: Option<f64>,
    ) -> DriverResult<NGValue> {
        if words.is_empty() {
            return Err(Self::cold_err("Empty register words"));
        }

        // --- Step 1: Normalize Registers to Byte Stream ---
        let mut bytes = Self::words_to_bytes(words, byte_order, word_order);
        let len = bytes.len();

        // --- Step 2: Handle Non-Numeric Types ---
        match data_type {
            DataType::Boolean => {
                // Convention: Non-zero value in the first register is true
                let b = words.first().map(|&w| w != 0).unwrap_or(false);
                return Ok(NGValue::Boolean(b));
            }
            DataType::String => {
                while let Some(true) = bytes.last().map(|b| *b == 0) {
                    bytes.pop();
                }
                let s = String::from_utf8_lossy(&bytes);
                return Ok(NGValue::String(Arc::<str>::from(s)));
            }
            DataType::Binary => return Ok(NGValue::Binary(Bytes::from(bytes))),
            DataType::Timestamp => {
                if len >= 8 {
                    let arr: [u8; 8] = bytes[0..8]
                        .try_into()
                        .map_err(|_| Self::cold_err("Failed to read i64 timestamp"))?;
                    return Ok(NGValue::Timestamp(i64::from_be_bytes(arr)));
                } else if len >= 4 {
                    // Fallback for 32-bit timestamps if encountered
                    let arr: [u8; 4] = bytes[0..4]
                        .try_into()
                        .map_err(|_| Self::cold_err("Failed to read u32 timestamp"))?;
                    let ts = u32::from_be_bytes(arr) as i64 * 1000;
                    return Ok(NGValue::Timestamp(ts));
                } else {
                    return Err(Self::cold_err("Insufficient bytes for timestamp"));
                }
            }
            _ => {} // Continue to numeric pipeline
        }

        // --- Step 3: Decode Numeric Types (Deterministic Matching) ---
        // We read everything as f64 first for uniform scaling.
        let raw_val: f64 = match (data_type, len) {
            // --- Float32 Target ---
            (DataType::Float32, 8..) => {
                // Interpret as f64 source (8 bytes) and later coerce to Float32.
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read f64 for Float32 target"))?;
                f64::from_be_bytes(arr)
            }
            (DataType::Float32, 4..) => {
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read f32"))?;
                f32::from_be_bytes(arr) as f64
            }
            (DataType::Float32, 2..) => {
                // Smart Cast: i16 -> f32
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i16 for Float32 target"))?;
                i16::from_be_bytes(arr) as f64
            }

            // --- Float64 Target ---
            (DataType::Float64, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read f64"))?;
                f64::from_be_bytes(arr)
            }
            (DataType::Float64, 4..) => {
                // Promote f32 -> f64
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read f32 for Float64 target"))?;
                f32::from_be_bytes(arr) as f64
            }
            (DataType::Float64, 2..) => {
                // Promote i16 -> f64
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i16 for Float64 target"))?;
                i16::from_be_bytes(arr) as f64
            }

            // --- Int32 Target ---
            (DataType::Int32, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i64 for Int32 target"))?;
                i64::from_be_bytes(arr) as f64
            }
            (DataType::Int32, 4..) => {
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i32"))?;
                i32::from_be_bytes(arr) as f64
            }
            (DataType::Int32, 2..) => {
                // Promote i16 -> i32
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i16 for Int32 target"))?;
                i16::from_be_bytes(arr) as f64
            }

            // --- UInt32 Target ---
            (DataType::UInt32, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u64 for UInt32 target"))?;
                u64::from_be_bytes(arr) as f64
            }
            (DataType::UInt32, 4..) => {
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u32"))?;
                u32::from_be_bytes(arr) as f64
            }
            (DataType::UInt32, 2..) => {
                // Promote u16 -> u32
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u16 for UInt32 target"))?;
                u16::from_be_bytes(arr) as f64
            }

            // --- Int64 Target ---
            (DataType::Int64, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i64"))?;
                i64::from_be_bytes(arr) as f64
            }
            (DataType::Int64, 4..) => {
                // Promote i32 -> i64
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i32 for Int64 target"))?;
                i32::from_be_bytes(arr) as f64
            }
            (DataType::Int64, 2..) => {
                // Promote i16 -> i64
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i16 for Int64 target"))?;
                i16::from_be_bytes(arr) as f64
            }

            // --- UInt64 Target ---
            (DataType::UInt64, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u64"))?;
                u64::from_be_bytes(arr) as f64
            }
            (DataType::UInt64, 4..) => {
                // Promote u32 -> u64
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u32 for UInt64 target"))?;
                u32::from_be_bytes(arr) as f64
            }
            (DataType::UInt64, 2..) => {
                // Promote u16 -> u64
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u16 for UInt64 target"))?;
                u16::from_be_bytes(arr) as f64
            }

            // --- Standard 16-bit / 8-bit Types ---
            (DataType::Int16, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i64 for Int16 target"))?;
                i64::from_be_bytes(arr) as f64
            }
            (DataType::Int16, 4..) => {
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i32 for Int16 target"))?;
                i32::from_be_bytes(arr) as f64
            }
            (DataType::Int16, 2..) => {
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i16"))?;
                i16::from_be_bytes(arr) as f64
            }

            (DataType::UInt16, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u64 for UInt16 target"))?;
                u64::from_be_bytes(arr) as f64
            }
            (DataType::UInt16, 4..) => {
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u32 for UInt16 target"))?;
                u32::from_be_bytes(arr) as f64
            }
            (DataType::UInt16, 2..) => {
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u16"))?;
                u16::from_be_bytes(arr) as f64
            }

            (DataType::Int8, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i64 for Int8 target"))?;
                i64::from_be_bytes(arr) as f64
            }
            (DataType::Int8, 4..) => {
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i32 for Int8 target"))?;
                i32::from_be_bytes(arr) as f64
            }
            (DataType::Int8, 2..) => {
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read i16 for Int8 target"))?;
                i16::from_be_bytes(arr) as f64
            }

            (DataType::UInt8, 8..) => {
                let arr: [u8; 8] = bytes[0..8]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u64 for UInt8 target"))?;
                u64::from_be_bytes(arr) as f64
            }
            (DataType::UInt8, 4..) => {
                let arr: [u8; 4] = bytes[0..4]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u32 for UInt8 target"))?;
                u32::from_be_bytes(arr) as f64
            }
            (DataType::UInt8, 2..) => {
                let arr: [u8; 2] = bytes[0..2]
                    .try_into()
                    .map_err(|_| Self::cold_err("Failed to read u16 for UInt8 target"))?;
                u16::from_be_bytes(arr) as f64
            }

            // --- Fallback / Error ---
            (dt, l) => {
                return Err(Self::cold_err_string(format!(
                    "Insufficient bytes ({}) for type {:?}",
                    l, dt
                )))
            }
        };

        // --- Step 4: Coerce and Box ---
        // ValueCodec::coerce_f64_to_value handles scaling, rounding, and type validation/casting
        ValueCodec::coerce_f64_to_value(raw_val, data_type, scale).ok_or_else(|| {
            Self::cold_err_string(format!(
                "Value {} out of range for {:?}",
                raw_val, data_type
            ))
        })
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
    /// The `quantity` parameter allows adapting the encoding to the actual register size
    /// expected by the device.
    /// - If `quantity` implies fewer bytes than the value, we attempt to cast/demote.
    /// - If `quantity` implies more bytes, we zero-pad.
    pub fn encode_registers_from_value(
        value: &NGValue,
        data_type: DataType,
        byte_order: Endianness,
        word_order: Endianness,
        quantity: u16,
    ) -> DriverResult<Vec<u16>> {
        // Special handling for String and Binary to use optimized paths
        if let DataType::String = data_type {
            if let NGValue::String(s) = value {
                return Ok(Self::encode_string_to_registers(
                    s,
                    byte_order,
                    word_order,
                    Some(quantity as usize),
                ));
            }
        }
        if let DataType::Binary = data_type {
            if let NGValue::Binary(b) = value {
                // For Binary, quantity implies max length or specific length?
                // Usually binary is just a blob. We can pad if needed.
                let mut words = Self::bytes_to_words_from_slice(b, byte_order, word_order);
                let q_usize = quantity as usize;
                if words.len() > q_usize {
                    words.truncate(q_usize);
                } else {
                    words.resize(q_usize, 0);
                }
                return Ok(words);
            }
        }

        let target_bytes = (quantity as usize) * 2;

        // 1. Convert Value to raw bytes (standard big-endian)
        // We use a strategy that matches the `quantity` first, then falls back to default.
        let mut raw_bytes: Vec<u8> = match (data_type, quantity) {
            // --- Float32 Source ---
            (DataType::Float32, 4) => {
                let v = f32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as f64).to_be_bytes().to_vec()
            }
            (DataType::Float32, 2) => {
                let v = f32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }
            (DataType::Float32, 1) => {
                let v = f32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let r = v.round();
                if !r.is_finite() {
                    return Err(Self::cast_err(value, "Float32 is not finite"));
                }
                if r < i16::MIN as f32 || r > i16::MAX as f32 {
                    return Err(Self::cast_err(value, "out of i16 range"));
                }
                (r as i16).to_be_bytes().to_vec()
            }

            // --- Float64 Source ---
            (DataType::Float64, 4) => {
                let v = f64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }
            (DataType::Float64, 2) => {
                let v = f64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                if !v.is_finite() {
                    return Err(Self::cast_err(value, "Float64 is not finite"));
                }
                if v < -(f32::MAX as f64) || v > f32::MAX as f64 {
                    return Err(Self::cast_err(value, "out of f32 range"));
                }
                (v as f32).to_be_bytes().to_vec()
            }
            (DataType::Float64, 1) => {
                let v = f64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let r = v.round();
                if !r.is_finite() {
                    return Err(Self::cast_err(value, "Float64 is not finite"));
                }
                if r < i16::MIN as f64 || r > i16::MAX as f64 {
                    return Err(Self::cast_err(value, "out of i16 range"));
                }
                (r as i16).to_be_bytes().to_vec()
            }

            // --- Int32 Source ---
            (DataType::Int32, 4) => {
                let v = i32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as i64).to_be_bytes().to_vec()
            }
            (DataType::Int32, 2) => {
                let v = i32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }
            (DataType::Int32, 1) => {
                let v = i32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let v16: i16 = v
                    .try_into()
                    .map_err(|_| Self::cast_err(value, "out of i16 range"))?;
                v16.to_be_bytes().to_vec()
            }

            // --- UInt32 Source ---
            (DataType::UInt32, 4) => {
                let v = u32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as u64).to_be_bytes().to_vec()
            }
            (DataType::UInt32, 2) => {
                let v = u32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }
            (DataType::UInt32, 1) => {
                let v = u32::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let v16: u16 = v
                    .try_into()
                    .map_err(|_| Self::cast_err(value, "out of u16 range"))?;
                v16.to_be_bytes().to_vec()
            }

            // --- Int64 Source ---
            (DataType::Int64, 4) => {
                let v = i64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }
            (DataType::Int64, 2) => {
                let v = i64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let v32: i32 = v
                    .try_into()
                    .map_err(|_| Self::cast_err(value, "out of i32 range"))?;
                v32.to_be_bytes().to_vec()
            }
            (DataType::Int64, 1) => {
                let v = i64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let v16: i16 = v
                    .try_into()
                    .map_err(|_| Self::cast_err(value, "out of i16 range"))?;
                v16.to_be_bytes().to_vec()
            }

            // --- UInt64 Source ---
            (DataType::UInt64, 4) => {
                let v = u64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }
            (DataType::UInt64, 2) => {
                let v = u64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let v32: u32 = v
                    .try_into()
                    .map_err(|_| Self::cast_err(value, "out of u32 range"))?;
                v32.to_be_bytes().to_vec()
            }
            (DataType::UInt64, 1) => {
                let v = u64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let v16: u16 = v
                    .try_into()
                    .map_err(|_| Self::cast_err(value, "out of u16 range"))?;
                v16.to_be_bytes().to_vec()
            }

            // --- Int16 Source ---
            (DataType::Int16, 4) => {
                let v = i16::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as i64).to_be_bytes().to_vec()
            }
            (DataType::Int16, 2) => {
                let v = i16::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as i32).to_be_bytes().to_vec()
            }
            (DataType::Int16, 1) => {
                let v = i16::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }

            // --- UInt16 Source ---
            (DataType::UInt16, 4) => {
                let v = u16::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as u64).to_be_bytes().to_vec()
            }
            (DataType::UInt16, 2) => {
                let v = u16::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as u32).to_be_bytes().to_vec()
            }
            (DataType::UInt16, 1) => {
                let v = u16::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                v.to_be_bytes().to_vec()
            }

            // --- Int8 Source ---
            (DataType::Int8, 4) => {
                let v = i8::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as i64).to_be_bytes().to_vec()
            }
            (DataType::Int8, 2) => {
                let v = i8::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as i32).to_be_bytes().to_vec()
            }
            (DataType::Int8, 1) => {
                let v = i8::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as i16).to_be_bytes().to_vec()
            }

            // --- UInt8 Source ---
            (DataType::UInt8, 4) => {
                let v = u8::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as u64).to_be_bytes().to_vec()
            }
            (DataType::UInt8, 2) => {
                let v = u8::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as u32).to_be_bytes().to_vec()
            }
            (DataType::UInt8, 1) => {
                let v = u8::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                (v as u16).to_be_bytes().to_vec()
            }

            // --- Boolean Source ---
            (DataType::Boolean, 4) => {
                let b = bool::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let n: u64 = if b { 1 } else { 0 };
                n.to_be_bytes().to_vec()
            }
            (DataType::Boolean, 2) => {
                let b = bool::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let n: u32 = if b { 1 } else { 0 };
                n.to_be_bytes().to_vec()
            }
            (DataType::Boolean, 1) => {
                let b = bool::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let n: u16 = if b { 1 } else { 0 };
                n.to_be_bytes().to_vec()
            }

            // --- Timestamp Source ---
            (DataType::Timestamp, 4) => {
                // Standard 64-bit ms
                let ms: i64 = i64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                ms.to_be_bytes().to_vec()
            }
            (DataType::Timestamp, 2) => {
                // 32-bit: Convert ms to seconds (u32)
                // Symmetric to parse logic: u32 * 1000 -> i64
                let ms: i64 = i64::try_from(value).map_err(|e| Self::cast_err(value, e))?;
                let seconds = ms / 1000;
                let s: u32 = seconds
                    .try_into()
                    .map_err(|_| Self::cast_err(value, "timestamp seconds out of u32 range"))?;
                s.to_be_bytes().to_vec()
            }

            // Catch-all fallthrough for unhandled permutations (e.g. q=3)
            // We use standard encoding and let the pad/check logic handle it.
            (_, _) => {
                return Err(DriverError::ValidationError(format!(
                    "Unsupported type/quantity combination: {:?} q={}",
                    data_type, quantity
                )))
            }
        };

        // 2. Adjust Length (Validate or Pad)
        if raw_bytes.len() > target_bytes {
            // If we are here, it means the default encoding produced more bytes than the target allows,
            // AND we didn't catch it in the special cases above.
            // Example: Writing Int32 (4 bytes) to Int16 (2 bytes).
            // This is ambiguous: do we drop high or low bytes?
            // To be safe, we reject it unless we add explicit truncation logic.
            return Err(DriverError::ValidationError(format!(
                "Value size {} bytes exceeds target size {} bytes (quantity={})",
                raw_bytes.len(),
                target_bytes,
                quantity
            )));
        }

        // Pad with zeros if needed (Big Endian -> pad at start)
        // E.g. writing i16 (2 bytes) to i32 slot (4 bytes) -> [0, 0, b0, b1]
        if raw_bytes.len() < target_bytes {
            let pad_len = target_bytes - raw_bytes.len();
            let mut padded: Vec<u8> = Vec::with_capacity(target_bytes);
            padded.resize(pad_len, 0);
            padded.extend_from_slice(&raw_bytes);
            raw_bytes = padded;
        }

        Ok(Self::bytes_to_words_from_slice(
            &raw_bytes, byte_order, word_order,
        ))
    }

    fn cast_err(val: &NGValue, err: impl std::fmt::Display) -> DriverError {
        DriverError::ValidationError(format!("Cast failed for {:?}: {}", val.data_type(), err))
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
