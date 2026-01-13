use anyhow::Result;
use byteorder::{LittleEndian, ReadBytesExt};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use std::io::Cursor;

// CP56Time2a , CP24Time2a, CP16Time2a
// |         Milliseconds(D7--D0)        | Milliseconds = 0-59999
// |         Milliseconds(D15--D8)       |
// | IV(D7)   RES1(D6)  Minutes(D5--D0)  | Minutes = 1-59, IV = invalid,0 = valid, 1 = invalid
// | SU(D7)   RES2(D6-D5)  Hours(D4--D0) | Hours = 0-23, SU = summer Time,0 = standard time, 1 = summer time,
// | DayOfWeek(D7--D5) DayOfMonth(D4--D0)| DayOfMonth = 1-31  DayOfWeek = 1-7
// | RES3(D7--D4)        Months(D3--D0)  | Months = 1-12
// | RES4(D7)            Year(D6--D0)    | Year = 0-99

pub fn cp56time2a(time: DateTime<Utc>) -> Bytes {
    let mut buf = BytesMut::with_capacity(8);

    let msec = (time.nanosecond() / 1000000) as u16 + time.second() as u16 * 1000;
    let minute = time.minute() as u8;
    let hour = time.hour() as u8;
    let weekday = time.weekday().number_from_monday() as u8;
    let day = time.day() as u8;
    let month = time.month() as u8;
    let year = (time.year() - 2000) as u8;

    buf.put_u16_le(msec);
    buf.put_u8(minute);
    buf.put_u8(hour);
    buf.put_u8(weekday << 5 | day);
    buf.put_u8(month);
    buf.put_u8(year);

    buf.freeze()
}

pub fn cp24time2a(time: DateTime<Utc>) -> Bytes {
    let mut buf = BytesMut::with_capacity(3);

    let msec = (time.nanosecond() / 1000000) as u16 + time.second() as u16 * 1000;
    let minute = time.minute() as u8;

    buf.put_u16_le(msec);
    buf.put_u8(minute);

    buf.freeze()
}

pub fn cp16time2a(time: DateTime<Utc>) -> Bytes {
    let mut buf = BytesMut::with_capacity(2);

    let msec = (time.nanosecond() / 1000000) as u16 + time.second() as u16 * 1000;

    buf.put_u16_le(msec);
    buf.freeze()
}

pub fn cp16time2a_from_msec(msec: u16) -> Bytes {
    let mut buf = BytesMut::with_capacity(2);
    buf.put_u16_le(msec);
    buf.freeze()
}

// decode info object byte to CP56Time2a
pub fn decode_cp56time2a(rdr: &mut Cursor<&Bytes>) -> Result<Option<DateTime<Utc>>> {
    if rdr.remaining() < 7 {
        return Ok(None);
    }
    let millisecond = rdr.read_u16::<LittleEndian>()?;
    let _msec = millisecond % 1000;
    let sec = (millisecond / 1000) as u32;
    let min = rdr.read_u8()?;
    let invalid = min & 0x80;
    let min = (min & 0x3f) as u32;
    let hour = (rdr.read_u8()? & 0x1f) as u32;
    let day = (rdr.read_u8()? & 0x1f) as u32;
    let month = (rdr.read_u8()? & 0x0f) as u32;
    let year = 2000 + (rdr.read_u8()? & 0x7f) as i32;

    if invalid != 0 {
        Ok(None)
    } else {
        Ok(Some(
            Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
                .unwrap(),
        ))
    }
}

// Decodecode info object byte to CP24Time2a
pub fn decode_cp24time2a(rdr: &mut Cursor<&Bytes>) -> Result<Option<DateTime<Utc>>> {
    if rdr.remaining() < 3 {
        return Ok(None);
    }
    let millisecond = rdr.read_u16::<LittleEndian>()?;
    let _msec = millisecond % 1000;
    let sec = (millisecond / 1000) as u32;
    let min = rdr.read_u8()?;
    let invalid = min & 0x80;
    let min = (min & 0x3f) as u32;

    let now_utc = Utc::now();
    let hour = now_utc.hour();
    let day = now_utc.day();
    let month = now_utc.month();
    let year = now_utc.year();
    if invalid != 0 {
        Ok(None)
    } else {
        Ok(Some(
            Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
                .unwrap(),
        ))
    }
}
