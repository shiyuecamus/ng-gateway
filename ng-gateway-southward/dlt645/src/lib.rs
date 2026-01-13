// DL/T 645 southward driver library entry.
//
// This crate implements a production-grade DL/T 645 protocol driver for the
// `ng-gateway` project. It follows the same dynamic loading and
// metadata conventions as other built-in southward drivers (e.g. Modbus, S7, MC).

mod codec;
mod driver;
mod factory;
mod metadata;
mod supervisor;
mod types;

pub mod protocol;
pub use driver::Dl645Driver;
use ng_gateway_sdk::ng_driver_factory;
pub use types::{
    Dl645Action, Dl645Channel, Dl645ChannelConfig, Dl645Connection, Dl645Device, Dl645FunctionCode,
    Dl645Parameter, Dl645Point, Dl645Version,
};

use crate::factory::Dl645DriverFactory;
use crate::metadata::build_metadata;

// Define the DL/T 645 driver factory and export metadata for dynamic loading.
//
// This macro generates all required C ABI symbols so that the gateway core can
// discover and load the driver as a plugin at runtime. The `driver_type`
// string is used as the unique identifier when registering the driver.
ng_driver_factory!(
    name = "DL/T645",
    description = "DL/T645 protocol driver",
    driver_type = "dlt645",
    factory = Dl645DriverFactory,
    metadata_fn = build_metadata
);
