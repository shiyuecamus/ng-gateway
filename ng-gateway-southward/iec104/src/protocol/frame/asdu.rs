use anyhow::{anyhow, Result};
use bit_struct::*;
use byteorder::ReadBytesExt;
use bytes::{BufMut, Bytes, BytesMut};
use std::{
    fmt::{Debug, Display},
    io::Cursor,
};

// ASDUSizeMax asdu max size
pub(crate) const ASDU_SIZE_MAX: usize = 249;

// ASDU format
//       | data unit identification | information object <1..n> |
//
//       | <------------  data unit identification ------------>|
//       | typeID | variable struct | cause  |  common address  |
// bytes |    1   |      1          | [1,2]  |      [1,2]       |
//       | <------------  information object ------------------>|
//       | object address | element set  |  object time scale   |
// bytes |     [1,2,3]    |              |                      |

// InvalidCommonAddr is the invalid common address.
pub const INVALID_COMMON_ADDR: u16 = 0;

// GlobalCommonAddr is the broadcast address. Use is restricted
// to C_IC_NA_1, C_CI_NA_1, C_CS_NA_1 and C_RP_NA_1.
// When in 8-bit mode 255 is mapped to this value on the fly.
#[allow(dead_code)]
const GLOBAL_COMMON_ADDR: u16 = 255;

pub const IDENTIFIER_SIZE: usize = 6;

pub type OriginAddr = u8;
pub type CommonAddr = u16;

#[derive(Debug, Clone)]
pub struct Asdu {
    pub identifier: Identifier,
    pub raw: Bytes,
}

impl Display for Asdu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.identifier.to_string().as_str())?;
        let mut s = String::with_capacity(self.raw.len() * 4);
        for b in self.raw.iter() {
            use std::fmt::Write as _;
            let _ = write!(&mut s, "[{:#04X}]", b);
        }
        f.write_str(&s)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Identifier {
    pub type_id: TypeID,
    pub variable_struct: VariableStruct,
    pub cot: CauseOfTransmission,
    // (一般不使用, 置0)
    pub orig_addr: OriginAddr,
    // (1~254为站地址, 255为全局地址, 0不使用)
    pub common_addr: CommonAddr,
}

impl Display for Identifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("[{:02X}]", self.type_id as u8))?;
        f.write_fmt(format_args!("[{:02X}]", self.variable_struct.raw()))?;
        f.write_fmt(format_args!("[{:02X}]", self.cot.raw()))?;
        f.write_fmt(format_args!("[{:02X}]", self.orig_addr))?;
        let common_addr = self.common_addr.to_le_bytes();
        f.write_fmt(format_args!("[{:02X}]", common_addr[0]))?;
        f.write_fmt(format_args!("[{:02X}]", common_addr[1]))?;
        Ok(())
    }
}

bit_struct! {
    pub struct VariableStruct(u8) {
        is_sequence: u1,
        number: u7,
    }
}

enums! {
    pub Cause {
        Unused,
        Periodic,
        Background,
        Spontaneous,
        Initialized,
        Request,
        Activation,
        ActivationCon,
        Deactivation,
        DeactivationCon,
        ActivationTerm,
        ReturnInfoRemote,
        ReturnInfoLocal,
        FileTransfer,
        Authentication,
        SessionKey,
        UserRoleAndUpdateKey,
        Reserved1,
        Reserved2,
        Reserved3,
        InterrogatedByStation,
        InterrogatedByGroup1,
        InterrogatedByGroup2,
        InterrogatedByGroup3,
        InterrogatedByGroup4,
        InterrogatedByGroup5,
        InterrogatedByGroup6,
        InterrogatedByGroup7,
        InterrogatedByGroup8,
        InterrogatedByGroup9,
        InterrogatedByGroup10,
        InterrogatedByGroup11,
        InterrogatedByGroup12,
        InterrogatedByGroup13,
        InterrogatedByGroup14,
        InterrogatedByGroup15,
        InterrogatedByGroup16,
        RequestByGeneralCounter,
        RequestByGroup1Counter,
        RequestByGroup2Counter,
        RequestByGroup3Counter,
        RequestByGroup4Counter,
        Reserved4,
        Reserved5,
        UnknownTypeID,
        UnknownCOT,
        UnknownCA,
        UnknownIOA,
    }
}

bit_struct! {
    pub struct CauseOfTransmission(u8) {
        test: bool,
        positive: bool,
        cause: Cause,
    }
}

#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum TypeID {
    M_SP_NA_1 = 1,
    M_SP_TA_1 = 2,
    M_DP_NA_1 = 3,
    M_DP_TA_1 = 4,
    M_ST_NA_1 = 5,
    M_ST_TA_1 = 6,
    M_BO_NA_1 = 7,
    M_BO_TA_1 = 8,
    M_ME_NA_1 = 9,
    M_ME_TA_1 = 10,
    M_ME_NB_1 = 11,
    M_ME_TB_1 = 12,
    M_ME_NC_1 = 13,
    M_ME_TC_1 = 14,
    M_IT_NA_1 = 15,
    M_IT_TA_1 = 16,
    M_EP_TA_1 = 17,
    M_EP_TB_1 = 18,
    M_EP_TC_1 = 19,
    M_PS_NA_1 = 20,
    M_ME_ND_1 = 21,
    M_SP_TB_1 = 30,
    M_DP_TB_1 = 31,
    M_ST_TB_1 = 32,
    M_BO_TB_1 = 33,
    M_ME_TD_1 = 34,
    M_ME_TE_1 = 35,
    M_ME_TF_1 = 36,
    M_IT_TB_1 = 37,
    M_EP_TD_1 = 38,
    M_EP_TE_1 = 39,
    M_EP_TF_1 = 40,
    S_IT_TC_1 = 41,
    C_SC_NA_1 = 45,
    C_DC_NA_1 = 46,
    C_RC_NA_1 = 47,
    C_SE_NA_1 = 48,
    C_SE_NB_1 = 49,
    C_SE_NC_1 = 50,
    C_BO_NA_1 = 51,
    C_SC_TA_1 = 58,
    C_DC_TA_1 = 59,
    C_RC_TA_1 = 60,
    C_SE_TA_1 = 61,
    C_SE_TB_1 = 62,
    C_SE_TC_1 = 63,
    C_BO_TA_1 = 64,
    M_EI_NA_1 = 70,
    S_CH_NA_1 = 81,
    S_RP_NA_1 = 82,
    S_AR_NA_1 = 83,
    S_KR_NA_1 = 84,
    S_KS_NA_1 = 85,
    S_KC_NA_1 = 86,
    S_ER_NA_1 = 87,
    S_US_NA_1 = 90,
    S_UQ_NA_1 = 91,
    S_UR_NA_1 = 92,
    S_UK_NA_1 = 93,
    S_UA_NA_1 = 94,
    S_UC_NA_1 = 95,
    C_IC_NA_1 = 100,
    C_CI_NA_1 = 101,
    C_RD_NA_1 = 102,
    C_CS_NA_1 = 103,
    C_TS_NA_1 = 104,
    C_RP_NA_1 = 105,
    C_CD_NA_1 = 106,
    C_TS_TA_1 = 107,
    P_ME_NA_1 = 110,
    P_ME_NB_1 = 111,
    P_ME_NC_1 = 112,
    P_AC_NA_1 = 113,
    F_FR_NA_1 = 120,
    F_SR_NA_1 = 121,
    F_SC_NA_1 = 122,
    F_LS_NA_1 = 123,
    F_AF_NA_1 = 124,
    F_SG_NA_1 = 125,
    F_DR_TA_1 = 126,
    F_SC_NB_1 = 127,
}

impl TypeID {
    #[inline]
    pub fn needs_time(&self) -> bool {
        matches!(
            *self,
            Self::C_SC_TA_1
                | Self::C_DC_TA_1
                | Self::C_RC_TA_1
                | Self::C_BO_TA_1
                | Self::C_SE_TA_1
                | Self::C_SE_TB_1
                | Self::C_SE_TC_1
        )
    }

    #[inline]
    pub fn map_to_command(&self) -> Option<Self> {
        // Already command types (accept as-is for compatibility).
        match *self {
            Self::C_SC_NA_1
            | Self::C_SC_TA_1
            | Self::C_DC_NA_1
            | Self::C_DC_TA_1
            | Self::C_RC_NA_1
            | Self::C_RC_TA_1
            | Self::C_BO_NA_1
            | Self::C_BO_TA_1
            | Self::C_SE_NA_1
            | Self::C_SE_TA_1
            | Self::C_SE_NB_1
            | Self::C_SE_TB_1
            | Self::C_SE_NC_1
            | Self::C_SE_TC_1 => return Some(*self),
            _ => {}
        }

        // Measurement types (M_*) mapped to command types (C_*), per design doc.
        Some(match *self {
            // Single point
            Self::M_SP_NA_1 => Self::C_SC_NA_1,
            Self::M_SP_TA_1 | Self::M_SP_TB_1 => Self::C_SC_TA_1,
            // Double point
            Self::M_DP_NA_1 => Self::C_DC_NA_1,
            Self::M_DP_TA_1 | Self::M_DP_TB_1 => Self::C_DC_TA_1,
            // Step
            Self::M_ST_NA_1 => Self::C_RC_NA_1,
            Self::M_ST_TA_1 | Self::M_ST_TB_1 => Self::C_RC_TA_1,
            // 32-bit bitstring
            Self::M_BO_NA_1 => Self::C_BO_NA_1,
            Self::M_BO_TA_1 | Self::M_BO_TB_1 => Self::C_BO_TA_1,
            // Measurements: Normalized/Scaled/Float
            Self::M_ME_NA_1 | Self::M_ME_ND_1 => Self::C_SE_NA_1,
            Self::M_ME_TA_1 | Self::M_ME_TD_1 => Self::C_SE_TA_1,
            Self::M_ME_NB_1 => Self::C_SE_NB_1,
            Self::M_ME_TB_1 | Self::M_ME_TE_1 => Self::C_SE_TB_1,
            Self::M_ME_NC_1 => Self::C_SE_NC_1,
            Self::M_ME_TC_1 | Self::M_ME_TF_1 => Self::C_SE_TC_1,
            _ => return None,
        })
    }
}

impl TryFrom<u8> for TypeID {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::M_SP_NA_1),
            2 => Ok(Self::M_SP_TA_1),
            3 => Ok(Self::M_DP_NA_1),
            4 => Ok(Self::M_DP_TA_1),
            5 => Ok(Self::M_ST_NA_1),
            6 => Ok(Self::M_ST_TA_1),
            7 => Ok(Self::M_BO_NA_1),
            8 => Ok(Self::M_BO_TA_1),
            9 => Ok(Self::M_ME_NA_1),
            10 => Ok(Self::M_ME_TA_1),
            11 => Ok(Self::M_ME_NB_1),
            12 => Ok(Self::M_ME_TB_1),
            13 => Ok(Self::M_ME_NC_1),
            14 => Ok(Self::M_ME_TC_1),
            15 => Ok(Self::M_IT_NA_1),
            16 => Ok(Self::M_IT_TA_1),
            17 => Ok(Self::M_EP_TA_1),
            18 => Ok(Self::M_EP_TB_1),
            19 => Ok(Self::M_EP_TC_1),
            20 => Ok(Self::M_PS_NA_1),
            21 => Ok(Self::M_ME_ND_1),
            30 => Ok(Self::M_SP_TB_1),
            31 => Ok(Self::M_DP_TB_1),
            32 => Ok(Self::M_ST_TB_1),
            33 => Ok(Self::M_BO_TB_1),
            34 => Ok(Self::M_ME_TD_1),
            35 => Ok(Self::M_ME_TE_1),
            36 => Ok(Self::M_ME_TF_1),
            37 => Ok(Self::M_IT_TB_1),
            38 => Ok(Self::M_EP_TD_1),
            39 => Ok(Self::M_EP_TE_1),
            40 => Ok(Self::M_EP_TF_1),
            41 => Ok(Self::S_IT_TC_1),
            45 => Ok(Self::C_SC_NA_1),
            46 => Ok(Self::C_DC_NA_1),
            47 => Ok(Self::C_RC_NA_1),
            48 => Ok(Self::C_SE_NA_1),
            49 => Ok(Self::C_SE_NB_1),
            50 => Ok(Self::C_SE_NC_1),
            51 => Ok(Self::C_BO_NA_1),
            52 => Ok(Self::M_IT_TA_1),
            53 => Ok(Self::M_IT_TA_1),
            54 => Ok(Self::M_IT_TA_1),
            55 => Ok(Self::M_IT_TA_1),
            56 => Ok(Self::M_IT_TA_1),
            57 => Ok(Self::M_IT_TA_1),
            58 => Ok(Self::C_SC_TA_1),
            59 => Ok(Self::C_DC_TA_1),
            60 => Ok(Self::C_RC_TA_1),
            61 => Ok(Self::C_SE_TA_1),
            62 => Ok(Self::C_SE_TB_1),
            63 => Ok(Self::C_SE_TC_1),
            64 => Ok(Self::C_BO_TA_1),
            70 => Ok(Self::M_EI_NA_1),
            81 => Ok(Self::S_CH_NA_1),
            82 => Ok(Self::S_RP_NA_1),
            83 => Ok(Self::S_AR_NA_1),
            84 => Ok(Self::S_KR_NA_1),
            85 => Ok(Self::S_KS_NA_1),
            86 => Ok(Self::S_KC_NA_1),
            87 => Ok(Self::S_ER_NA_1),
            90 => Ok(Self::S_US_NA_1),
            91 => Ok(Self::S_UQ_NA_1),
            92 => Ok(Self::S_UR_NA_1),
            93 => Ok(Self::S_UK_NA_1),
            94 => Ok(Self::S_UA_NA_1),
            95 => Ok(Self::S_UC_NA_1),
            100 => Ok(Self::C_IC_NA_1),
            101 => Ok(Self::C_CI_NA_1),
            102 => Ok(Self::C_RD_NA_1),
            103 => Ok(Self::C_CS_NA_1),
            104 => Ok(Self::C_TS_NA_1),
            105 => Ok(Self::C_RP_NA_1),
            106 => Ok(Self::C_CD_NA_1),
            107 => Ok(Self::C_TS_TA_1),
            110 => Ok(Self::P_ME_NA_1),
            111 => Ok(Self::P_ME_NB_1),
            112 => Ok(Self::P_ME_NC_1),
            113 => Ok(Self::P_AC_NA_1),
            120 => Ok(Self::F_FR_NA_1),
            121 => Ok(Self::F_SR_NA_1),
            122 => Ok(Self::F_SC_NA_1),
            123 => Ok(Self::F_LS_NA_1),
            124 => Ok(Self::F_AF_NA_1),
            125 => Ok(Self::F_SG_NA_1),
            126 => Ok(Self::F_DR_TA_1),
            127 => Ok(Self::F_SC_NB_1),
            _ => Err(anyhow!("Unknown TypeId: {}", value)),
        }
    }
}

// 信息对象地址 (IEC104)
bit_struct! {
    pub struct InfoObjAddr(u24) {
        res: u8,       // 未使用, 置0
        addr: u16,     // 有效取值 [1, 65534]
    }
}

// InfoObjAddrIrrelevant Zero means that the information object address is irrelevant.
pub const INFO_OBJ_ADDR_IRRELEVANT: u16 = 0;

impl Asdu {
    pub fn mirror(&self, cause: Cause) -> Self {
        let mut asdu = self.clone();
        asdu.identifier.cot.cause().set(cause);
        asdu
    }
}

impl TryFrom<Bytes> for Asdu {
    type Error = anyhow::Error;

    fn try_from(bytes: Bytes) -> Result<Self> {
        let mut rdr = Cursor::new(&bytes);
        let type_id = TypeID::try_from(rdr.read_u8()?)?;
        let variable_struct = VariableStruct::try_from(rdr.read_u8()?)
            .map_err(|_| anyhow!("invalid variable struct"))?;
        let cot = CauseOfTransmission::try_from(rdr.read_u8()?)
            .map_err(|_| anyhow!("invalid cause of transmission"))?;
        let orig_addr = rdr.read_u8()?;
        let common_addr = rdr.read_u16::<byteorder::LittleEndian>()?;
        let mut bytes = bytes;
        Ok(Asdu {
            identifier: Identifier {
                type_id,
                variable_struct,
                cot,
                orig_addr,
                common_addr,
            },
            raw: bytes.split_off(IDENTIFIER_SIZE),
        })
    }
}

impl TryInto<Bytes> for Asdu {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<Bytes, Self::Error> {
        // Reserve exact capacity to avoid over-allocation
        let cap = IDENTIFIER_SIZE + self.raw.len();
        let mut buf = BytesMut::with_capacity(cap);
        buf.put_u8(self.identifier.type_id as u8);
        buf.put_u8(self.identifier.variable_struct.raw());
        buf.put_u8(self.identifier.cot.raw());
        buf.put_u8(self.identifier.orig_addr);
        buf.put_u16_le(self.identifier.common_addr);
        buf.extend(self.raw);

        Ok(buf.freeze())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_and_encode_asdu() -> Result<()> {
        let bytes =
            Bytes::from_static(&[0x01, 0x01, 0x06, 0x00, 0x80, 0x00, 0x00, 0x01, 0x02, 0x03]);
        let mut asdu: Asdu = bytes.clone().try_into()?;
        assert!(asdu.identifier.type_id == TypeID::M_SP_NA_1);
        assert_eq!(asdu.identifier.variable_struct.number().get().value(), 0x01);
        assert_eq!(asdu.identifier.cot.cause().get(), Cause::Activation);
        assert_eq!(asdu.identifier.orig_addr, 0x00);
        assert_eq!(asdu.identifier.common_addr, 0x80);
        assert_eq!(asdu.raw, Bytes::from_static(&[0x00, 0x01, 0x02, 0x03]));

        let raw: Bytes = asdu.try_into().unwrap();
        assert_eq!(bytes, raw);
        Ok(())
    }
}
