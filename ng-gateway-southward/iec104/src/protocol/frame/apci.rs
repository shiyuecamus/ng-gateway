use super::{
    asdu::{Asdu, ASDU_SIZE_MAX, IDENTIFIER_SIZE},
    Apdu,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::{collections::VecDeque, fmt::Display};

pub const START_FRAME: u8 = 0x68; // 启动字符

// APDU form Max size 255
//      |              APCI                   |       ASDU         |
//      | start | APDU length | control field |       ASDU         |
//                       |          APDU field size(253)           |
// bytes|    1  |    1   |        4           |                    |
pub const APCI_FIELD_SIZE: usize = 6;
pub const APCICTL_FIELD_SIZE: usize = 4;
pub const APDU_SIZE_MAX: usize = 255;
pub const APDU_FIELD_SIZE_MAX: usize = APCICTL_FIELD_SIZE + ASDU_SIZE_MAX;

// U帧 控制域功能
pub const U_STARTDT_ACTIVE: u8 = 0x04; // 启动激活
pub const U_STARTDT_CONFIRM: u8 = 0x08; // 启动确认
pub const U_STOPDT_ACTIVE: u8 = 0x10; // 停止激活
pub const U_STOPDT_CONFIRM: u8 = 0x20; // 停止确认
pub const U_TESTFR_ACTIVE: u8 = 0x40; // 测试激活
pub const U_TESTFR_CONFIRM: u8 = 0x80; // 测试确认

#[derive(Debug, Clone, Copy)]
pub struct Apci {
    pub start: u8,
    pub apdu_length: u8,
    pub ctrl1: u8,
    pub ctrl2: u8,
    pub ctrl3: u8,
    pub ctrl4: u8,
}

impl Display for Apci {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("[{:02X}]", self.start))?;
        f.write_fmt(format_args!("[{:02X}]", self.apdu_length))?;
        f.write_fmt(format_args!("[{:02X}]", self.ctrl1))?;
        f.write_fmt(format_args!("[{:02X}]", self.ctrl2))?;
        f.write_fmt(format_args!("[{:02X}]", self.ctrl3))?;
        f.write_fmt(format_args!("[{:02X}]", self.ctrl4))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IApci {
    pub send_sn: u16,
    pub rcv_sn: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct UApci {
    pub function: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct SApci {
    pub rcv_sn: u16,
}

pub enum ApciKind {
    I(IApci),
    U(UApci),
    S(SApci),
}

#[derive(Debug, Clone)]
pub struct SeqPending {
    pub seq: u16,
    pub send_time: DateTime<Utc>,
    pub size_bytes: usize,
}

impl From<Apci> for ApciKind {
    fn from(apci: Apci) -> Self {
        if apci.ctrl1 & 0x01 == 0 {
            return ApciKind::I(IApci {
                send_sn: ((apci.ctrl1 as u16) >> 1) + ((apci.ctrl2 as u16) << 7),
                rcv_sn: ((apci.ctrl3 as u16) >> 1) + ((apci.ctrl4 as u16) << 7),
            });
        }

        if apci.ctrl1 & 0x03 == 0x01 {
            return ApciKind::S(SApci {
                rcv_sn: ((apci.ctrl3 as u16) >> 1) + ((apci.ctrl4 as u16) << 7),
            });
        }

        ApciKind::U(UApci {
            function: apci.ctrl1 & 0xfc,
        })
    }
}

#[inline]
pub fn new_iframe(asdu: Asdu, send_sn: u16, rcv_sn: u16) -> Apdu {
    let apci = Apci {
        start: START_FRAME,
        apdu_length: APCICTL_FIELD_SIZE as u8 + IDENTIFIER_SIZE as u8 + asdu.raw.len() as u8,
        ctrl1: (send_sn << 1) as u8,
        ctrl2: (send_sn >> 7) as u8,
        ctrl3: (rcv_sn << 1) as u8,
        ctrl4: (rcv_sn >> 7) as u8,
    };
    Apdu {
        apci,
        asdu: Some(asdu),
    }
}

#[inline]
/// Compute the on-wire byte size for an I-frame carrying the given ASDU.
/// This includes the full APCI field (start + length + 4-byte control),
/// the ASDU identifier bytes, and the ASDU raw payload size.
pub fn iframe_wire_size_for_asdu(asdu: &Asdu) -> usize {
    APCI_FIELD_SIZE + IDENTIFIER_SIZE + asdu.raw.len()
}

#[inline]
pub fn new_sframe(rcv_sn: u16) -> Apdu {
    Apdu {
        apci: Apci {
            start: START_FRAME,
            apdu_length: APCICTL_FIELD_SIZE as u8,
            ctrl1: 0x01,
            ctrl2: 0x00,
            ctrl3: (rcv_sn << 1) as u8,
            ctrl4: (rcv_sn >> 7) as u8,
        },
        asdu: None,
    }
}

#[inline]
pub fn new_uframe(function: u8) -> Apdu {
    Apdu {
        apci: Apci {
            start: START_FRAME,
            apdu_length: APCICTL_FIELD_SIZE as u8,
            ctrl1: function | 0x03,
            ctrl2: 0x00,
            ctrl3: 0x00,
            ctrl4: 0x00,
        },
        asdu: None,
    }
}

#[inline]
fn seq_no_count(next_ack_no: u16, mut next_send_no: u16) -> u16 {
    if next_ack_no > next_send_no {
        next_send_no += 32768;
    }
    next_send_no - next_ack_no
}

#[inline]
pub fn update_ack_no_out(
    ack_no: u16,
    ack_sendsn: &mut u16,
    send_sn: &mut u16,
    pending: &mut VecDeque<SeqPending>,
) -> bool {
    if ack_no == *ack_sendsn {
        return true;
    }

    if seq_no_count(*ack_sendsn, *send_sn) < seq_no_count(ack_no, *send_sn) {
        return false;
    }

    for _ in 0..pending.len() {
        if let Some(p) = pending.pop_front() {
            if p.seq == ack_no - 1 {
                break;
            }
        }
    }
    *ack_sendsn = ack_no;
    true
}
