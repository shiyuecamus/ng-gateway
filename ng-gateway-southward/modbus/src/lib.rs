mod codec;
pub mod driver;
mod factory;
mod metadata;
mod planner;
mod supervisor;
pub mod types;

pub use driver::ModbusDriver;

use factory::ModbusDriverFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "Modbus",
    description = "Modbus RTU/TCP industrial protocol driver",
    driver_type = "modbus",
    factory = ModbusDriverFactory,
    metadata_fn = build_metadata
);
