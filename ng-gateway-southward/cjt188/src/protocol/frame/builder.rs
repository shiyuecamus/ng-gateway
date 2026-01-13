use super::defs::{Cjt188Address, MeterType};
use super::{
    body::Cjt188Body,
    defs::{ControlWord, DataIdentifier, Direction, FunctionCode},
    Cjt188TypedFrame,
};
use bytes::Bytes;

/// Build a CJ/T 188 read data request frame.
///
/// * `address`: Target meter address.
/// * `di`: Data Identifier (2 bytes).
/// * `serial`: Frame serial number (SER/SEQ), 1 byte.
pub fn build_read_data_frame(
    meter_type: MeterType,
    address: Cjt188Address,
    di: DataIdentifier,
    serial: u8,
) -> Cjt188TypedFrame {
    // Control: Master->Slave, ReadData (0x01)
    let control = ControlWord::from_parts(
        Direction::MasterToSlave,
        false,
        false,
        FunctionCode::ReadData,
    );
    let body = Cjt188Body::ReadData { di, serial };
    Cjt188TypedFrame::new(meter_type, address, control, body)
}

/// Build a CJ/T 188 write data request frame.
///
/// * `address`: Target meter address.
/// * `di`: Data Identifier.
/// * `serial`: Frame serial number (SER/SEQ), 1 byte.
/// * `value_bytes`: Raw value bytes to write.
pub fn build_write_data_frame(
    meter_type: MeterType,
    address: Cjt188Address,
    di: DataIdentifier,
    serial: u8,
    value_bytes: Vec<u8>,
) -> Cjt188TypedFrame {
    // Control: Master->Slave, WriteData (0x04)
    let control = ControlWord::from_parts(
        Direction::MasterToSlave,
        false,
        false,
        FunctionCode::WriteData,
    );

    let body = Cjt188Body::WriteData {
        di,
        serial,
        value_bytes: Bytes::from(value_bytes),
    };

    Cjt188TypedFrame::new(meter_type, address, control, body)
}

/// Build a CJ/T 188 write motor sync request frame.
///
/// * `address`: Target meter address.
/// * `di`: Data Identifier.
/// * `serial`: Frame serial number (SER/SEQ), 1 byte.
/// * `value_bytes`: Raw value bytes to write.
pub fn build_write_motor_sync_frame(
    meter_type: MeterType,
    address: Cjt188Address,
    di: DataIdentifier,
    serial: u8,
    value_bytes: Vec<u8>,
) -> Cjt188TypedFrame {
    // Control: Master->Slave, WriteMotorSync (0x16)
    let control = ControlWord::from_parts(
        Direction::MasterToSlave,
        false,
        false,
        FunctionCode::WriteMotorSync,
    );

    let body = Cjt188Body::WriteMotorSync {
        di,
        serial,
        value_bytes: Bytes::from(value_bytes),
    };

    Cjt188TypedFrame::new(meter_type, address, control, body)
}

/// Build a CJ/T 188 read address request frame.
///
/// Function Code: 0x03
pub fn build_read_address_frame(serial: u8) -> Cjt188TypedFrame {
    // NOTE: For "read address" request, standard practice is using broadcast address bytes (AA).
    // Meter type is set to `0xAA` by default to maximize compatibility across mixed meter types.
    // If your deployment requires a specific type (e.g. water only), consider adding a typed
    // overload and passing `MeterType::COLD_WATER` etc.
    let meter_type = MeterType::from(0xAA);
    let address = Cjt188Address::broadcast_aa();
    let control = ControlWord::from_parts(
        Direction::MasterToSlave,
        false,
        false,
        FunctionCode::ReadAddr,
    );
    // User requirement: Read Address DI is fixed 810AH
    let di = DataIdentifier::from(0x810A);
    let body = Cjt188Body::ReadAddr { di, serial };
    Cjt188TypedFrame::new(meter_type, address, control, body)
}

/// Build a CJ/T 188 write address request frame.
///
/// * `current_address`: The address currently on the meter (or broadcast).
/// * `new_address`: The new address to write.
/// * `serial`: Frame serial number (SER/SEQ), 1 byte.
///   Function Code: 0x15
pub fn build_write_address_frame(
    meter_type: MeterType,
    current_address: Cjt188Address,
    new_address: Cjt188Address,
    serial: u8,
) -> Cjt188TypedFrame {
    let control = ControlWord::from_parts(
        Direction::MasterToSlave,
        false,
        false,
        FunctionCode::WriteAddr,
    );
    // Write Addr request uses Data Identifier 0xA018 (usually implicit or strictly needed? Standard says DI is part of body)
    // We'll use Address DI for this.
    let di = DataIdentifier::Address;
    let body = Cjt188Body::WriteAddr {
        di,
        serial,
        new_address,
    };
    Cjt188TypedFrame::new(meter_type, current_address, control, body)
}
