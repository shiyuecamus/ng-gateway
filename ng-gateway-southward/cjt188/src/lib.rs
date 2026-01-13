pub mod codec;
pub mod driver;
mod factory;
mod metadata;
pub mod protocol;
pub mod supervisor;
pub mod types;

pub use driver::Cjt188Driver;
use factory::Cjt188DriverFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Define the CJ/T 188 driver factory and export metadata for dynamic loading.
//
// This macro generates all required C ABI symbols so that the gateway core can
// discover and load the driver as a plugin at runtime. The `driver_type`
// string is used as the unique identifier when registering the driver.
ng_driver_factory!(
    name = "CJ/T 188",
    description = "CJ/T 188 protocol driver",
    driver_type = "cjt188",
    factory = Cjt188DriverFactory,
    metadata_fn = build_metadata
);
