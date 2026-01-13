use super::{
    super::error::{Error, Result},
    types::{S7Area, S7TransportSize},
};
use serde::{Deserialize, Serialize};

/// Strongly typed S7 address with pre-parsed fields for zero-cost reuse.
///
/// This new type avoids repeatedly parsing string addresses. It also provides
/// `TryFrom<&str>` for ergonomic construction while ensuring validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct S7Address {
    /// Parsed S7 memory area
    pub area: u8,
    /// Data block number when `area == DB`, else 0
    pub db_number: u16,
    /// Byte address within the area
    pub byte_address: u32,
    /// Bit index 0..=7 for bit-level access, else 0
    pub bit_index: u8,
    /// Transport size inferred from the textual form (Bit/Byte/Word/DWord...)
    pub transport_size: u8,
}

// Longest-first to avoid prefix shadowing (e.g. DI vs D, DATETIME vs DATE vs DT)
const TOKENS: &[(&str, S7TransportSize)] = &[
    ("DATETIME", S7TransportSize::DateTime),
    ("DATETIMELONG", S7TransportSize::DateTimeLong),
    ("S5TIME", S7TransportSize::S5Time),
    ("WSTRING", S7TransportSize::WString),
    ("STRING", S7TransportSize::String),
    ("DINT", S7TransportSize::DInt),
    ("DWORD", S7TransportSize::DWord),
    ("DW", S7TransportSize::DWord),
    ("CHAR", S7TransportSize::Char),
    ("BYTE", S7TransportSize::Byte),
    ("WORD", S7TransportSize::Word),
    ("TIME", S7TransportSize::Time),
    ("DATE", S7TransportSize::Date),
    ("REAL", S7TransportSize::Real),
    ("TOD", S7TransportSize::TimeOfDay),
    ("BIT", S7TransportSize::Bit),
    ("DT", S7TransportSize::DateTime),
    ("DTL", S7TransportSize::DateTimeLong),
    ("DI", S7TransportSize::DInt),
    ("INT", S7TransportSize::Int),
    ("ST", S7TransportSize::S5Time),
    ("WS", S7TransportSize::WString),
    ("S", S7TransportSize::String),
    ("R", S7TransportSize::Real),
    ("C", S7TransportSize::Char),
    ("W", S7TransportSize::Word),
    ("B", S7TransportSize::Byte),
    ("I", S7TransportSize::Int),
    ("X", S7TransportSize::Bit),
    ("T", S7TransportSize::Time),
];

/// Match typed token prefixes for transport sizes in a longest-first manner.
/// Supports both short and long forms, e.g. "X"/"BIT", "B"/"BYTE", "C"/"CHAR",
/// "D"/"DW"/"DWORD", "DI"/"DINT", "I"/"INT", "W"/"WORD", "R/REAL", "T"/"TIME", "DATE",
/// "DT"/"DATETIME", "TOD", "ST"/"S5TIME", "S"/"STRING", "WS"/"WSTRING".
#[inline]
fn match_type_token(s: &str) -> Option<(S7TransportSize, &str)> {
    for (tok, ts) in TOKENS {
        if let Some(rest) = s.strip_prefix(tok) {
            return Some((*ts, rest));
        }
    }
    None
}

/// Parse a typed tail like "BYTE10" or bit form like "BIT6.3" into an `S7Address`.
/// - For Bit, requires explicit ".bit" component.
/// - For non-bit types, forbids any dot component.
#[inline]
fn parse_typed_spec(area: S7Area, dbn: u16, tail: &str) -> Result<Option<S7Address>> {
    if let Some((ts, rest)) = match_type_token(tail) {
        if matches!(ts, S7TransportSize::Bit) {
            let mut addr = rest.splitn(2, '.');
            let byte = addr
                .next()
                .ok_or(Error::ErrInvalidFrame)?
                .parse::<u32>()
                .map_err(|_| Error::ErrInvalidFrame)
                .and_then(check_byte)?;
            let bit = addr
                .next()
                .ok_or(Error::ErrInvalidFrame)?
                .parse::<u8>()
                .map_err(|_| Error::ErrInvalidFrame)
                .and_then(check_bit)?;
            return Ok(Some(S7Address {
                transport_size: ts as u8,
                db_number: if matches!(area, S7Area::DB) { dbn } else { 0 },
                area: area as u8,
                byte_address: byte,
                bit_index: bit,
            }));
        } else {
            if rest.contains('.') {
                return Err(Error::ErrInvalidFrame);
            }
            let byte = rest
                .parse::<u32>()
                .map_err(|_| Error::ErrInvalidFrame)
                .and_then(check_byte)?;
            return Ok(Some(S7Address {
                transport_size: ts as u8,
                db_number: if matches!(area, S7Area::DB) { dbn } else { 0 },
                area: area as u8,
                byte_address: byte,
                bit_index: 0,
            }));
        }
    }
    Ok(None)
}

/// Parse with unified strategy for most areas:
/// 1) Try typed spec via `parse_typed_spec`.
/// 2) Fallback to `byte.bit` as Bit, if a dot is present.
/// 3) Fallback to plain `byte` as Byte, if non-empty.
#[inline]
fn parse_typed_fallback(area: S7Area, dbn: u16, rest: &str) -> Result<Option<S7Address>> {
    if let Some(spec) = parse_typed_spec(area, dbn, rest)? {
        return Ok(Some(spec));
    }
    if let Some((byte_s, bit_s)) = rest.split_once('.') {
        let byte = byte_s
            .parse::<u32>()
            .map_err(|_| Error::ErrInvalidFrame)
            .and_then(check_byte)?;
        let bit = bit_s
            .parse::<u8>()
            .map_err(|_| Error::ErrInvalidFrame)
            .and_then(check_bit)?;
        return Ok(Some(S7Address {
            transport_size: S7TransportSize::Bit as u8,
            db_number: if matches!(area, S7Area::DB) { dbn } else { 0 },
            area: area as u8,
            byte_address: byte,
            bit_index: bit,
        }));
    }
    if !rest.is_empty() {
        let byte = rest
            .parse::<u32>()
            .map_err(|_| Error::ErrInvalidFrame)
            .and_then(check_byte)?;
        return Ok(Some(S7Address {
            transport_size: S7TransportSize::Byte as u8,
            db_number: if matches!(area, S7Area::DB) { dbn } else { 0 },
            area: area as u8,
            byte_address: byte,
            bit_index: 0,
        }));
    }
    Ok(None)
}

/// Parse an S7 address string like "DB1.DWORD0", "DB1.WORD0", "DB1.BYTE0",
/// or bit-level like "DB1.BIT0.2", and peripheral areas like "I0.0", "Q0.1", "M10.3".
/// Returns a `S7Address` with transport size, area, db number, byte address and bit index.
pub fn parse_s7_address(input: &str) -> Result<S7Address> {
    // Normalize and basic validation
    let s = input.trim().to_uppercase();
    if s.is_empty() {
        return Err(Error::ErrInvalidParam);
    }

    // Try DB area first
    if let Some(spec) = parse_db_area(&s)? {
        return Ok(spec);
    }

    // DI / L / DP areas (multi-char / special first)
    if let Some(spec) = parse_di_area(&s)? {
        return Ok(spec);
    }
    if let Some(spec) = parse_l_area(&s)? {
        return Ok(spec);
    }
    if let Some(spec) = parse_dp_area(&s)? {
        return Ok(spec);
    }

    // Timer / Counter
    if let Some(spec) = parse_timer_counter(&s)? {
        return Ok(spec);
    }

    // Short areas: I/Q/M/V
    if let Some(spec) = parse_short_area(&s)? {
        return Ok(spec);
    }

    Err(Error::ErrInvalidAddress(input.to_string()))
}

#[inline]
fn parse_db_area(s: &str) -> Result<Option<S7Address>> {
    if let Some(rest) = s.strip_prefix("DB") {
        let mut parts = rest.splitn(2, '.');
        let dbn = parts
            .next()
            .ok_or(Error::ErrInvalidFrame)?
            .parse::<u16>()
            .map_err(|_| Error::ErrInvalidFrame)
            .and_then(check_db)?;
        let tail = parts.next().ok_or(Error::ErrInvalidFrame)?;
        // Prefer typed tokens like INT/WORD/REAL/etc.
        if let Some(spec) = parse_typed_fallback(S7Area::DB, dbn, tail)? {
            return Ok(Some(spec));
        }
        return Err(Error::ErrInvalidFrame);
    }
    Ok(None)
}

#[inline]
fn parse_di_area(s: &str) -> Result<Option<S7Address>> {
    if let Some(rest) = s.strip_prefix("DI") {
        return parse_typed_fallback(S7Area::DI, 0, rest);
    }
    Ok(None)
}

#[inline]
fn parse_l_area(s: &str) -> Result<Option<S7Address>> {
    if let Some(rest) = s.strip_prefix('L') {
        return parse_typed_fallback(S7Area::L, 0, rest);
    }
    Ok(None)
}

#[inline]
fn parse_dp_area(s: &str) -> Result<Option<S7Address>> {
    if let Some(rest) = s.strip_prefix("DP") {
        return parse_typed_fallback(S7Area::DP, 0, rest);
    }
    Ok(None)
}

#[inline]
fn parse_timer_counter(s: &str) -> Result<Option<S7Address>> {
    if let Some(rest) = s.strip_prefix('T') {
        let idx = rest.parse::<u32>().map_err(|_| Error::ErrInvalidFrame)?;
        let idx = check_byte(idx)?;
        return Ok(Some(S7Address {
            transport_size: S7TransportSize::Timer as u8,
            db_number: 0,
            area: S7Area::T as u8,
            byte_address: idx,
            bit_index: 0,
        }));
    }
    if let Some(rest) = s.strip_prefix('C') {
        let idx = rest.parse::<u32>().map_err(|_| Error::ErrInvalidFrame)?;
        let idx = check_byte(idx)?;
        return Ok(Some(S7Address {
            transport_size: S7TransportSize::Counter as u8,
            db_number: 0,
            area: S7Area::C as u8,
            byte_address: idx,
            bit_index: 0,
        }));
    }
    Ok(None)
}

#[inline]
fn parse_short_area(s: &str) -> Result<Option<S7Address>> {
    if !matches!(s.chars().next(), Some('I' | 'Q' | 'M' | 'V')) {
        return Ok(None);
    }
    let area = match &s[0..1] {
        "I" => S7Area::I,
        "Q" => S7Area::O,
        "M" => S7Area::M,
        "V" => S7Area::DB,
        _ => return Err(Error::ErrInvalidFrame),
    };
    parse_typed_fallback(
        area,
        if matches!(area, S7Area::DB) { 1 } else { 0 },
        &s[1..],
    )
}

// Small helpers
#[inline]
fn check_db(n: u16) -> Result<u16> {
    if (1..=64000).contains(&n) {
        Ok(n)
    } else {
        Err(Error::ErrInvalidAddress(format!("db out of range: {n}")))
    }
}

#[inline]
fn check_byte(n: u32) -> Result<u32> {
    if n <= 2_097_151 {
        Ok(n)
    } else {
        Err(Error::ErrInvalidAddress(format!("byte out of range: {n}")))
    }
}

#[inline]
fn check_bit(n: u8) -> Result<u8> {
    if n <= 7 {
        Ok(n)
    } else {
        Err(Error::ErrInvalidAddress(format!("bit out of range: {n}")))
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{S7Area, S7TransportSize};
    use super::parse_s7_address;
    use super::S7Address;

    /// Create a `S7VarSpec` from address string or panic with useful context.
    fn addr(input: &str) -> S7Address {
        match parse_s7_address(input) {
            Ok(s) => s,
            Err(e) => panic!("parse_s7_address failed for '{input}': {e:?}"),
        }
    }

    /// Full coverage for typed prefixes and their short aliases across DB/I/M/Q areas as required.
    #[test]
    fn test_parse_s7_address_all() {
        // Helper to assert one address quickly
        let assert_addr =
            |s: &str, ts: S7TransportSize, area: S7Area, db: u16, byte: u32, bit: u8| {
                let a = addr(s);
                assert_eq!(a.transport_size, ts as u8, "ts for {s}");
                assert_eq!(a.area, area as u8, "area for {s}");
                assert_eq!(a.db_number, db, "db for {s}");
                assert_eq!(a.byte_address, byte, "byte for {s}");
                assert_eq!(a.bit_index, bit, "bit for {s}");
            };

        // Bit: DB and short areas
        assert_addr("DB1.BIT1.1", S7TransportSize::Bit, S7Area::DB, 1, 1, 1);
        assert_addr("DB1.X1.1", S7TransportSize::Bit, S7Area::DB, 1, 1, 1);
        assert_addr("IX1.1", S7TransportSize::Bit, S7Area::I, 0, 1, 1);
        assert_addr("IBIT1.1", S7TransportSize::Bit, S7Area::I, 0, 1, 1);
        assert_addr("MX1.1", S7TransportSize::Bit, S7Area::M, 0, 1, 1);
        assert_addr("MBIT1.1", S7TransportSize::Bit, S7Area::M, 0, 1, 1);
        assert_addr("QX1.1", S7TransportSize::Bit, S7Area::O, 0, 1, 1);
        assert_addr("QBIT1.1", S7TransportSize::Bit, S7Area::O, 0, 1, 1);

        // Byte
        assert_addr("DB1.BYTE1", S7TransportSize::Byte, S7Area::DB, 1, 1, 0);
        assert_addr("IB1", S7TransportSize::Byte, S7Area::I, 0, 1, 0);
        assert_addr("IBYTE1", S7TransportSize::Byte, S7Area::I, 0, 1, 0);
        assert_addr("MB1", S7TransportSize::Byte, S7Area::M, 0, 1, 0);
        assert_addr("MBYTE1", S7TransportSize::Byte, S7Area::M, 0, 1, 0);
        assert_addr("QB1", S7TransportSize::Byte, S7Area::O, 0, 1, 0);
        assert_addr("QBYTE1", S7TransportSize::Byte, S7Area::O, 0, 1, 0);

        // Char
        assert_addr("DB1.C1", S7TransportSize::Char, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.CHAR1", S7TransportSize::Char, S7Area::DB, 1, 1, 0);
        assert_addr("IC1", S7TransportSize::Char, S7Area::I, 0, 1, 0);
        assert_addr("ICHAR1", S7TransportSize::Char, S7Area::I, 0, 1, 0);
        assert_addr("MC1", S7TransportSize::Char, S7Area::M, 0, 1, 0);
        assert_addr("MCHAR1", S7TransportSize::Char, S7Area::M, 0, 1, 0);
        assert_addr("QC1", S7TransportSize::Char, S7Area::O, 0, 1, 0);
        assert_addr("QCHAR1", S7TransportSize::Char, S7Area::O, 0, 1, 0);

        // DWord
        assert_addr("DB1.DWORD1", S7TransportSize::DWord, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.DW16", S7TransportSize::DWord, S7Area::DB, 1, 16, 0);
        assert_addr("IDWORD1", S7TransportSize::DWord, S7Area::I, 0, 1, 0);
        assert_addr("IDW16", S7TransportSize::DWord, S7Area::I, 0, 16, 0);
        assert_addr("MDWORD1", S7TransportSize::DWord, S7Area::M, 0, 1, 0);
        assert_addr("MDW16", S7TransportSize::DWord, S7Area::M, 0, 16, 0);
        assert_addr("QDWORD1", S7TransportSize::DWord, S7Area::O, 0, 1, 0);
        assert_addr("QDW16", S7TransportSize::DWord, S7Area::O, 0, 16, 0);

        // DInt
        assert_addr("DB1.DI1", S7TransportSize::DInt, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.DINT1", S7TransportSize::DInt, S7Area::DB, 1, 1, 0);
        assert_addr("IDI1", S7TransportSize::DInt, S7Area::I, 0, 1, 0);
        assert_addr("IDINT1", S7TransportSize::DInt, S7Area::I, 0, 1, 0);
        assert_addr("MDI1", S7TransportSize::DInt, S7Area::M, 0, 1, 0);
        assert_addr("MDINT1", S7TransportSize::DInt, S7Area::M, 0, 1, 0);
        assert_addr("QDI1", S7TransportSize::DInt, S7Area::O, 0, 1, 0);
        assert_addr("QDINT1", S7TransportSize::DInt, S7Area::O, 0, 1, 0);

        // Int / Word
        assert_addr("DB1.I1", S7TransportSize::Int, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.INT1", S7TransportSize::Int, S7Area::DB, 1, 1, 0);
        assert_addr("II1", S7TransportSize::Int, S7Area::I, 0, 1, 0);
        assert_addr("IINT1", S7TransportSize::Int, S7Area::I, 0, 1, 0);
        assert_addr("MI1", S7TransportSize::Int, S7Area::M, 0, 1, 0);
        assert_addr("MINT1", S7TransportSize::Int, S7Area::M, 0, 1, 0);
        assert_addr("QI1", S7TransportSize::Int, S7Area::O, 0, 1, 0);
        assert_addr("QINT1", S7TransportSize::Int, S7Area::O, 0, 1, 0);

        assert_addr("DB1.W1", S7TransportSize::Word, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.WORD1", S7TransportSize::Word, S7Area::DB, 1, 1, 0);
        assert_addr("IW1", S7TransportSize::Word, S7Area::I, 0, 1, 0);
        assert_addr("IWORD1", S7TransportSize::Word, S7Area::I, 0, 1, 0);
        assert_addr("MW1", S7TransportSize::Word, S7Area::M, 0, 1, 0);
        assert_addr("MWORD1", S7TransportSize::Word, S7Area::M, 0, 1, 0);
        assert_addr("QW1", S7TransportSize::Word, S7Area::O, 0, 1, 0);
        assert_addr("QWORD1", S7TransportSize::Word, S7Area::O, 0, 1, 0);

        // Real
        assert_addr("DB1.REAL1", S7TransportSize::Real, S7Area::DB, 1, 1, 0);
        assert_addr("IREAL1", S7TransportSize::Real, S7Area::I, 0, 1, 0);
        assert_addr("MREAL1", S7TransportSize::Real, S7Area::M, 0, 1, 0);
        assert_addr("QREAL1", S7TransportSize::Real, S7Area::O, 0, 1, 0);

        // Time
        assert_addr("DB1.T1", S7TransportSize::Time, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.TIME1", S7TransportSize::Time, S7Area::DB, 1, 1, 0);
        assert_addr("IT1", S7TransportSize::Time, S7Area::I, 0, 1, 0);
        assert_addr("ITIME1", S7TransportSize::Time, S7Area::I, 0, 1, 0);
        assert_addr("MT1", S7TransportSize::Time, S7Area::M, 0, 1, 0);
        assert_addr("MTIME1", S7TransportSize::Time, S7Area::M, 0, 1, 0);
        assert_addr("QT1", S7TransportSize::Time, S7Area::O, 0, 1, 0);
        assert_addr("QTIME1", S7TransportSize::Time, S7Area::O, 0, 1, 0);

        // Date
        assert_addr("DB1.DATE1", S7TransportSize::Date, S7Area::DB, 1, 1, 0);
        assert_addr("IDATE1", S7TransportSize::Date, S7Area::I, 0, 1, 0);
        assert_addr("MDATE1", S7TransportSize::Date, S7Area::M, 0, 1, 0);
        assert_addr("QDATE1", S7TransportSize::Date, S7Area::O, 0, 1, 0);

        // DateTime
        assert_addr("DB1.DT1", S7TransportSize::DateTime, S7Area::DB, 1, 1, 0);
        assert_addr(
            "DB1.DATETIME1",
            S7TransportSize::DateTime,
            S7Area::DB,
            1,
            1,
            0,
        );
        assert_addr("IDT1", S7TransportSize::DateTime, S7Area::I, 0, 1, 0);
        assert_addr("IDATETIME1", S7TransportSize::DateTime, S7Area::I, 0, 1, 0);
        assert_addr("MDT1", S7TransportSize::DateTime, S7Area::M, 0, 1, 0);
        assert_addr("MDATETIME1", S7TransportSize::DateTime, S7Area::M, 0, 1, 0);
        assert_addr("QDT1", S7TransportSize::DateTime, S7Area::O, 0, 1, 0);
        assert_addr("QDATETIME1", S7TransportSize::DateTime, S7Area::O, 0, 1, 0);

        // TimeOfDay
        assert_addr("DB1.TOD1", S7TransportSize::TimeOfDay, S7Area::DB, 1, 1, 0);
        assert_addr("ITOD1", S7TransportSize::TimeOfDay, S7Area::I, 0, 1, 0);
        assert_addr("MTOD1", S7TransportSize::TimeOfDay, S7Area::M, 0, 1, 0);
        assert_addr("QTOD1", S7TransportSize::TimeOfDay, S7Area::O, 0, 1, 0);

        // S5Time
        assert_addr("DB1.ST1", S7TransportSize::S5Time, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.S5TIME1", S7TransportSize::S5Time, S7Area::DB, 1, 1, 0);
        assert_addr("IST1", S7TransportSize::S5Time, S7Area::I, 0, 1, 0);
        assert_addr("IS5TIME1", S7TransportSize::S5Time, S7Area::I, 0, 1, 0);
        assert_addr("MST1", S7TransportSize::S5Time, S7Area::M, 0, 1, 0);
        assert_addr("MS5TIME1", S7TransportSize::S5Time, S7Area::M, 0, 1, 0);
        assert_addr("QST1", S7TransportSize::S5Time, S7Area::O, 0, 1, 0);
        assert_addr("QS5TIME1", S7TransportSize::S5Time, S7Area::O, 0, 1, 0);

        // String (mapped to String transport here)
        assert_addr("DB1.S1", S7TransportSize::String, S7Area::DB, 1, 1, 0);
        assert_addr("DB1.STRING1", S7TransportSize::String, S7Area::DB, 1, 1, 0);
        assert_addr("IS1", S7TransportSize::String, S7Area::I, 0, 1, 0);
        assert_addr("ISTRING1", S7TransportSize::String, S7Area::I, 0, 1, 0);
        assert_addr("MS1", S7TransportSize::String, S7Area::M, 0, 1, 0);
        assert_addr("MSTRING1", S7TransportSize::String, S7Area::M, 0, 1, 0);
        assert_addr("QS1", S7TransportSize::String, S7Area::O, 0, 1, 0);
        assert_addr("QSTRING1", S7TransportSize::String, S7Area::O, 0, 1, 0);

        assert_addr("DB1.WS1", S7TransportSize::WString, S7Area::DB, 1, 1, 0);
        assert_addr(
            "DB1.WSTRING1",
            S7TransportSize::WString,
            S7Area::DB,
            1,
            1,
            0,
        );
        assert_addr("IWS1", S7TransportSize::WString, S7Area::I, 0, 1, 0);
        assert_addr("IWSTRING1", S7TransportSize::WString, S7Area::I, 0, 1, 0);
        assert_addr("MWS1", S7TransportSize::WString, S7Area::M, 0, 1, 0);
        assert_addr("MWSTRING1", S7TransportSize::WString, S7Area::M, 0, 1, 0);
        assert_addr("QWS1", S7TransportSize::WString, S7Area::O, 0, 1, 0);
        assert_addr("QWSTRING1", S7TransportSize::WString, S7Area::O, 0, 1, 0);

        // A few invalids
        assert!(parse_s7_address("").is_err());
        assert!(parse_s7_address("DB1.INT0.1").is_err()); // dot not allowed for non-bit
        assert!(parse_s7_address("QX1").is_err()); // bit requires .bit
        assert!(parse_s7_address("DB70000.BYTE1").is_err()); // db out of range
    }
}
