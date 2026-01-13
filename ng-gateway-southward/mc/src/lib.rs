mod codec;
mod driver;
mod factory;
mod metadata;
mod protocol;
mod supervisor;
mod typed_api;
mod types;

use factory::McDriverFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "Melsec MC",
    description = "Mitsubishi PLC MC protocol driver",
    driver_type = "mc",
    factory = McDriverFactory,
    metadata_fn = build_metadata
);
