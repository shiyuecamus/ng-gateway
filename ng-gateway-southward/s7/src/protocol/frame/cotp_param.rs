use super::{
    super::error::{Error, Result},
    types::CotpType,
    WireDecode, WireEncode,
};
use bytes::{BufMut, Bytes};

/// Class/Option (CR/CC fixed field) decoded from a single byte
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassOption {
    /// High nibble (bits 7..=4): class identifier
    pub class_number: u8,
    /// Bit 1 (second from LSB): Extended formats flag
    pub extended_format: bool,
    /// Bit 0 (LSB): No explicit flow control flag
    pub no_explicit_flow_control: bool,
}

impl From<u8> for ClassOption {
    fn from(b: u8) -> Self {
        Self {
            class_number: (b >> 4) & 0x0F,
            extended_format: (b & 0b10) != 0,
            no_explicit_flow_control: (b & 0b01) != 0,
        }
    }
}

impl From<ClassOption> for u8 {
    fn from(val: ClassOption) -> Self {
        let mut b = (val.class_number & 0x0F) << 4;
        if val.extended_format {
            b |= 0b10;
        }
        if val.no_explicit_flow_control {
            b |= 0b01;
        }
        b
    }
}

/// COTP Connection Request parameters
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CotpCrParams {
    pub dst_ref: u16,
    pub src_ref: u16,
    pub class_option: ClassOption,
    /// TPDU Size (2^n bytes, typically 0x0A = 1024 bytes)
    pub tpdu_size: u8,
    /// Source TSAP (typically rack/slot info)
    pub src_tsap: u16,
    /// Destination TSAP (typically rack/slot info)  
    pub dst_tsap: u16,
}

impl Default for CotpCrParams {
    fn default() -> Self {
        Self {
            dst_ref: 0x0000,
            src_ref: 0x0001,
            class_option: ClassOption::default(),
            tpdu_size: 0x0A,  // 1024 bytes
            src_tsap: 0x0100, // Rack 1, Slot 0
            dst_tsap: 0x0100, // Rack 1, Slot 0
        }
    }
}

impl CotpCrParams {
    /// Convenience: decode `tpdu_size` (2^n) into bytes
    pub fn tpdu_size_bytes(&self) -> Option<usize> {
        tpdu_size_bytes_from_code(self.tpdu_size)
    }

    pub fn parse_body(body: &[u8]) -> Result<Self> {
        if body.len() < 5 {
            return Err(Error::ErrInvalidFrame);
        }

        let dst_ref = u16::from_be_bytes([body[0], body[1]]);
        let src_ref = u16::from_be_bytes([body[2], body[3]]);
        let class_option = body[4].into();
        let params_data = &body[5..];

        let (tpdu_size, src_tsap, dst_tsap) = parse_connection_params(params_data)?;

        Ok(CotpCrParams {
            dst_ref,
            src_ref,
            class_option,
            tpdu_size,
            src_tsap,
            dst_tsap,
        })
    }
}

impl WireEncode for CotpCrParams {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        2 + 2 + 1 + 11
    }
    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        dst.put_slice(&self.dst_ref.to_be_bytes());
        dst.put_slice(&self.src_ref.to_be_bytes());
        dst.put_u8(self.class_option.into());
        write_tlv(dst, 0xC0, &[self.tpdu_size]);
        write_tlv(dst, 0xC1, &self.src_tsap.to_be_bytes());
        write_tlv(dst, 0xC2, &self.dst_tsap.to_be_bytes());
        Ok(())
    }
}

impl WireDecode for CotpCrParams {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        let (pdu_type, body) = validate_and_split_header(input, Some(CotpType::Cr))?;
        debug_assert!(matches!(pdu_type, CotpType::Cr));
        let rest = &input[1 + body.len() + 1..];
        Ok((rest, CotpCrParams::parse_body(body)?))
    }
}

/// Return TLV on-wire length for value length `len`
#[inline]
pub fn tlv_len(len: usize) -> usize {
    2 + len
}

/// Write a TLV with `code` and raw `bytes`
#[inline]
pub fn write_tlv<B: bytes::BufMut>(dst: &mut B, code: u8, bytes: &[u8]) {
    dst.put_u8(code);
    dst.put_u8(bytes.len() as u8);
    dst.put_slice(bytes);
}

/// COTP Connection Confirm parameters
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CotpCcParams {
    pub dst_ref: u16,
    pub src_ref: u16,
    pub class_option: ClassOption,
    /// TPDU Size (2^n bytes, typically 0x0A = 1024 bytes)
    pub tpdu_size: u8,
    /// Source TSAP (typically rack/slot info)
    pub src_tsap: u16,
    /// Destination TSAP (typically rack/slot info)  
    pub dst_tsap: u16,
}

impl Default for CotpCcParams {
    fn default() -> Self {
        Self {
            dst_ref: 0x0001,
            src_ref: 0x0000,
            class_option: ClassOption::default(),
            tpdu_size: 0x0A,  // 1024 bytes
            src_tsap: 0x0100, // Rack 1, Slot 0
            dst_tsap: 0x0100, // Rack 1, Slot 0
        }
    }
}

impl CotpCcParams {
    /// Convenience: decode `tpdu_size` (2^n) into bytes
    pub fn tpdu_size_bytes(&self) -> Option<usize> {
        tpdu_size_bytes_from_code(self.tpdu_size)
    }

    pub fn parse_body(body: &[u8]) -> Result<Self> {
        if body.len() < 5 {
            return Err(Error::ErrInvalidFrame);
        }

        let dst_ref = u16::from_be_bytes([body[0], body[1]]);
        let src_ref = u16::from_be_bytes([body[2], body[3]]);
        let class_option = body[4].into();
        let params_data = &body[5..];

        let (tpdu_size, src_tsap, dst_tsap) = parse_connection_params(params_data)?;

        Ok(CotpCcParams {
            dst_ref,
            src_ref,
            class_option,
            tpdu_size,
            src_tsap,
            dst_tsap,
        })
    }
}

impl WireEncode for CotpCcParams {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        2 + 2 + 1 + 11
    }
    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        dst.put_slice(&self.dst_ref.to_be_bytes());
        dst.put_slice(&self.src_ref.to_be_bytes());
        dst.put_u8(self.class_option.into());
        write_tlv(dst, 0xC0, &[self.tpdu_size]);
        write_tlv(dst, 0xC1, &self.src_tsap.to_be_bytes());
        write_tlv(dst, 0xC2, &self.dst_tsap.to_be_bytes());
        Ok(())
    }
}

impl WireDecode for CotpCcParams {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        let (_ty, body) = validate_and_split_header(input, Some(CotpType::Cc))?;
        let rest = &input[1 + body.len() + 1..];
        Ok((rest, CotpCcParams::parse_body(body)?))
    }
}

// Removed legacy From/TryFrom impls; use WireEncode/WireDecode instead

/// COTP Disconnection Request parameters
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CotpDrParams {
    pub dst_ref: u16,
    pub src_ref: u16,
    pub class_option: ClassOption,
    /// Optional disconnect additional information parameter (0xE0)
    pub additional_info: Option<Vec<u8>>,
}

impl Default for CotpDrParams {
    fn default() -> Self {
        Self {
            dst_ref: 0x0001,
            src_ref: 0x0000,
            class_option: ClassOption::default(),
            additional_info: None,
        }
    }
}

impl CotpDrParams {
    pub fn parse_body(body: &[u8]) -> Result<Self> {
        if body.len() < 5 {
            return Err(Error::ErrInvalidFrame);
        }

        let dst_ref = u16::from_be_bytes([body[0], body[1]]);
        let src_ref = u16::from_be_bytes([body[2], body[3]]);
        let class_option = body[4].into();
        let mut pos = 5usize;
        let mut additional_info: Option<Vec<u8>> = None;
        while pos + 2 <= body.len() {
            let code = body[pos];
            let len = body[pos + 1] as usize;
            pos += 2;
            if pos + len > body.len() {
                return Err(Error::ErrInvalidFrame);
            }
            match code {
                0xE0 => {
                    additional_info = Some(body[pos..pos + len].to_vec());
                }
                _ => {
                    // skip unknown parameters
                }
            }
            pos += len;
        }

        Ok(CotpDrParams {
            dst_ref,
            src_ref,
            class_option,
            additional_info,
        })
    }
}

impl WireEncode for CotpDrParams {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        let tlv_len = match &self.additional_info {
            Some(v) if !v.is_empty() => 2 + v.len().min(u8::MAX as usize),
            _ => 0,
        };
        2 + 2 + 1 + tlv_len
    }
    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        dst.put_slice(&self.dst_ref.to_be_bytes());
        dst.put_slice(&self.src_ref.to_be_bytes());
        dst.put_u8(self.class_option.into());
        if let Some(v) = &self.additional_info {
            if !v.is_empty() {
                let len = v.len().min(u8::MAX as usize) as u8;
                dst.put_u8(0xE0);
                dst.put_u8(len);
                dst.put_slice(&v[..len as usize]);
            }
        }
        Ok(())
    }
}

impl WireDecode for CotpDrParams {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        let (_ty, body) = validate_and_split_header(input, Some(CotpType::Dr))?;
        let rest = &input[1 + body.len() + 1..];
        Ok((rest, CotpDrParams::parse_body(body)?))
    }
}

/// COTP Disconnection Confirm parameters
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CotpDcParams {
    pub dst_ref: u16,
    pub src_ref: u16,
}

impl Default for CotpDcParams {
    fn default() -> Self {
        Self {
            dst_ref: 0x0000,
            src_ref: 0x0001,
        }
    }
}

impl CotpDcParams {
    pub fn parse_body(body: &[u8]) -> Result<Self> {
        if body.len() < 4 {
            return Err(Error::ErrInvalidFrame);
        }
        let dst_ref = u16::from_be_bytes([body[0], body[1]]);
        let src_ref = u16::from_be_bytes([body[2], body[3]]);
        // tolerate trailing pad bytes
        Ok(CotpDcParams { dst_ref, src_ref })
    }
}

impl WireEncode for CotpDcParams {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        2 + 2
    }
    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        dst.put_slice(&self.dst_ref.to_be_bytes());
        dst.put_slice(&self.src_ref.to_be_bytes());
        Ok(())
    }
}

impl WireDecode for CotpDcParams {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        let (_ty, body) = validate_and_split_header(input, Some(CotpType::Dc))?;
        let rest = &input[1 + body.len() + 1..];
        Ok((rest, CotpDcParams::parse_body(body)?))
    }
}

/// COTP TPDU-Error/Reject parameters
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CotpReParams {
    pub dst_ref: u16,
    /// Reject cause
    pub cause: u8,
}

impl CotpReParams {
    pub fn parse_body(body: &[u8]) -> Result<Self> {
        if body.len() < 3 {
            return Err(Error::ErrInvalidFrame);
        }
        let dst_ref = u16::from_be_bytes([body[0], body[1]]);
        let cause = body[2];
        Ok(CotpReParams { dst_ref, cause })
    }
}

impl WireEncode for CotpReParams {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        2 + 1
    }
    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        dst.put_slice(&self.dst_ref.to_be_bytes());
        dst.put_u8(self.cause);
        Ok(())
    }
}

impl WireDecode for CotpReParams {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        let (_ty, body) = validate_and_split_header(input, Some(CotpType::Re))?;
        let rest = &input[1 + body.len() + 1..];
        Ok((rest, CotpReParams::parse_body(body)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CotpDataParams {
    pub eot: bool,
    pub tpdu_nr: u8,
}

impl Default for CotpDataParams {
    fn default() -> Self {
        Self {
            eot: true,
            tpdu_nr: 0,
        }
    }
}

impl CotpDataParams {
    pub fn parse_body(body: &[u8]) -> Result<Self> {
        if body.is_empty() {
            return Err(Error::ErrInvalidFrame);
        }

        let eot_nr = body[0];
        let eot = (eot_nr & 0x80) != 0;
        let tpdu_nr = eot_nr & 0x7F;

        Ok(CotpDataParams { eot, tpdu_nr })
    }
}

impl WireEncode for CotpDataParams {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        1
    }
    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        let eot_nr = if self.eot {
            0x80 | self.tpdu_nr
        } else {
            self.tpdu_nr
        };
        dst.put_u8(eot_nr);
        Ok(())
    }
}

impl WireDecode for CotpDataParams {
    type Error = Error;
    type Context = ();

    fn parse<'a>(
        input: &'a [u8],
        _parent: &Bytes,
        _ctx: &Self::Context,
    ) -> Result<(&'a [u8], Self)> {
        let (_ty, body) = validate_and_split_header(input, Some(CotpType::D))?;
        let rest = &input[1 + body.len() + 1..];
        Ok((rest, CotpDataParams::parse_body(body)?))
    }
}

/// Fixed overhead of a COTP Data TPDU header (bytes): LI(1) + Type(1) + DataParams(1)
pub const COTP_D_FIXED_HEADER_LEN: usize = 1 + 1 + 1;

/// Calculate the maximum user payload size (bytes) that can fit into a single COTP Data TPDU
/// for a given TPDU size code (2^n). Returns None for invalid/overflow cases.
#[inline]
pub fn calc_max_cotp_d_payload_bytes(tpdu_size_code: u8) -> Option<usize> {
    tpdu_size_bytes_from_code(tpdu_size_code)
        .map(|tpdu_bytes| tpdu_bytes.saturating_sub(COTP_D_FIXED_HEADER_LEN))
}

/// Map TPDU size code (2^n) to actual byte size. Returns None if it would overflow usize.
pub fn tpdu_size_bytes_from_code(code: u8) -> Option<usize> {
    if (code as u32) < usize::BITS {
        Some(1usize << code)
    } else {
        None
    }
}

/// Parse standard S7 connection parameters from parameter data
fn parse_connection_params(data: &[u8]) -> Result<(u8, u16, u16)> {
    let mut pos = 0;
    let mut tpdu_size = None;
    let mut src_tsap = None;
    let mut dst_tsap = None;

    // Parse parameter TLVs in any order, skip unknowns
    while pos + 2 <= data.len() {
        let code = data[pos];
        let len = data[pos + 1] as usize;
        pos += 2;

        if pos + len > data.len() {
            return Err(Error::ErrInvalidFrame);
        }

        match code {
            0xC0 if len == 1 => {
                tpdu_size = Some(data[pos]);
            }
            0xC1 if len == 2 => {
                src_tsap = Some(u16::from_be_bytes([data[pos], data[pos + 1]]));
            }
            0xC2 if len == 2 => {
                dst_tsap = Some(u16::from_be_bytes([data[pos], data[pos + 1]]));
            }
            _ => {
                // skip unknown parameter
            }
        }
        pos += len;
    }

    // All three standard parameters must be present
    match (tpdu_size, src_tsap, dst_tsap) {
        (Some(tpdu_size), Some(src_tsap), Some(dst_tsap)) => Ok((tpdu_size, src_tsap, dst_tsap)),
        _ => Err(Error::ErrInvalidFrame),
    }
}

/// Validate COTP header and return the parsed PDU type and body slice
fn validate_and_split_header(data: &[u8], expected: Option<CotpType>) -> Result<(CotpType, &[u8])> {
    if data.len() < 2 {
        return Err(Error::ErrInvalidFrame);
    }
    let li = data[0] as usize;
    let total = 1usize + li;
    if total > data.len() || li < 1 {
        return Err(Error::ErrInvalidFrame);
    }
    let tpdu_type_u8 = data[1];
    let pdu_type: CotpType = tpdu_type_u8
        .try_into()
        .map_err(|_| Error::ErrInvalidFrame)?;
    if let Some(expect) = expected {
        if pdu_type != expect {
            return Err(Error::ErrInvalidFrame);
        }
    }
    let body = &data[2..total];
    Ok((pdu_type, body))
}
