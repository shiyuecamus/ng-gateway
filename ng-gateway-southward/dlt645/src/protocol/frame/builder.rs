use super::{
    BroadcastTimeSyncBody, ClearEventsBody, ClearMaxDemandBody, ClearMeterBody, Dl645Address,
    Dl645Body, Dl645ControlWord, Dl645Function, Dl645TypedFrame, FreezeBody, ModifyPasswordBody,
    ReadDataBody, UpdateBaudRateBody, WriteAddressBody, WriteDataBody,
};
use crate::types::Dl645Version;
use bytes::Bytes;

/// Build a DL/T 645 read-data request frame.
pub fn build_read_data_frame(
    version: Dl645Version,
    address: Dl645Address,
    di: u32,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::ReadData);
    let body = Dl645Body::ReadData(ReadDataBody {
        di,
        data: Bytes::new(),
    });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 write-data request frame.
pub fn build_write_data_frame(
    version: Dl645Version,
    address: Dl645Address,
    di: u32,
    value_bytes: Vec<u8>,
    password: u32,
    operator_code: Option<u32>,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::WriteData);
    let body = Dl645Body::WriteData(WriteDataBody {
        di,
        value: Bytes::from(value_bytes),
        password,
        operator_code: operator_code.unwrap_or(0),
    });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 "write communication address" frame.
pub fn build_write_address_frame(
    version: Dl645Version,
    current_address: Dl645Address,
    new_address_bcd: Dl645Address,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::WriteAddress);
    let body = Dl645Body::WriteAddress(WriteAddressBody {
        new_address: new_address_bcd,
    });
    Dl645TypedFrame::new(current_address, control, body)
}

/// Build a DL/T 645 broadcast time-synchronization frame.
pub fn build_broadcast_time_sync_frame(
    version: Dl645Version,
    timestamp_bcd: Vec<u8>,
) -> Dl645TypedFrame {
    let address = Dl645Address([0x99u8; 6]);
    let control = Dl645ControlWord::for_request(version, Dl645Function::BroadcastTimeSync);
    let body = Dl645Body::BroadcastTimeSync(BroadcastTimeSyncBody {
        timestamp: Bytes::from(timestamp_bcd),
    });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 freeze command frame.
pub fn build_freeze_frame(
    version: Dl645Version,
    address: Dl645Address,
    pattern_bcd: Vec<u8>,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::Freeze);
    let body = Dl645Body::Freeze(FreezeBody {
        pattern: Bytes::from(pattern_bcd),
    });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 "update baud rate" frame.
pub fn build_update_baud_rate_frame(
    version: Dl645Version,
    address: Dl645Address,
    code: u8,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::UpdateBaudRate);
    let body = Dl645Body::UpdateBaudRate(UpdateBaudRateBody { code });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 "modify password" frame.
pub fn build_modify_password_frame(
    version: Dl645Version,
    address: Dl645Address,
    di: Option<u32>,
    old_password: u32,
    new_password: u32,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::ModifyPassword);
    let body = Dl645Body::ModifyPassword(ModifyPasswordBody {
        di,
        old_password,
        new_password,
    });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 "clear maximum demand" frame.
pub fn build_clear_max_demand_frame(
    version: Dl645Version,
    address: Dl645Address,
    password: u32,
    operator_code: Option<u32>,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::ClearMaxDemand);
    let body = Dl645Body::ClearMaxDemand(ClearMaxDemandBody {
        password,
        operator_code: operator_code.unwrap_or(0),
    });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 "clear meter" frame.
pub fn build_clear_meter_frame(
    version: Dl645Version,
    address: Dl645Address,
    password: u32,
    operator_code: Option<u32>,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::ClearMeter);
    let body = Dl645Body::ClearMeter(ClearMeterBody {
        password,
        operator_code: operator_code.unwrap_or(0),
    });
    Dl645TypedFrame::new(address, control, body)
}

/// Build a DL/T 645 "clear events" frame.
pub fn build_clear_events_frame(
    version: Dl645Version,
    address: Dl645Address,
    di: u32,
    password: u32,
    operator_code: Option<u32>,
) -> Dl645TypedFrame {
    let control = Dl645ControlWord::for_request(version, Dl645Function::ClearEvents);
    let body = Dl645Body::ClearEvents(ClearEventsBody {
        di,
        password,
        operator_code: operator_code.unwrap_or(0),
    });
    Dl645TypedFrame::new(address, control, body)
}
