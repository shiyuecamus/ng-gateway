mod codec;
mod driver;
mod factory;
mod metadata;
#[allow(unused)]
mod protocol;
mod supervisor;
mod types;

use factory::S7DriverFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "Siemens S7",
    description = "Siemens S7 protocol driver",
    driver_type = "s7",
    factory = S7DriverFactory,
    metadata_fn = build_metadata
);
