mod codec;
mod connector;
mod converter;
mod handle;
mod metadata;
mod planner;
mod session;
pub mod types;

pub use connector::ModbusConnector;
use converter::ModbusConverter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "Modbus",
    description = "Modbus RTU/TCP industrial protocol driver",
    driver_type = "modbus",
    component = ModbusConnector,
    metadata_fn = build_metadata,
    model_convert = ModbusConverter
);
