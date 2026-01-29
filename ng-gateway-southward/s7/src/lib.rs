mod codec;
mod connector;
mod converter;
mod handle;
mod metadata;
#[allow(unused)]
mod protocol;
mod session;
mod types;

use connector::S7Connector;
use converter::S7Converter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "Siemens S7",
    description = "Siemens S7 protocol driver",
    driver_type = "s7",
    component = S7Connector,
    metadata_fn = build_metadata,
    model_convert = S7Converter
);
