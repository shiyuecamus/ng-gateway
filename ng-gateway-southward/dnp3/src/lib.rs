mod codec;
pub mod driver;
mod factory;
mod handler;
mod metadata;
mod supervisor;
pub mod types;

use factory::Dnp3DriverFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "DNP3",
    description = "DNP3 protocol driver",
    driver_type = "dnp3",
    factory = Dnp3DriverFactory,
    metadata_fn = build_metadata
);
