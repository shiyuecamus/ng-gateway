mod driver;
mod factory;
mod metadata;
pub mod protocol;
mod supervisor;
mod types;

use factory::Iec104DriverFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "IEC 60870-5-104",
    description = "IEC104 protocol driver",
    driver_type = "iec104",
    factory = Iec104DriverFactory,
    metadata_fn = build_metadata
);
