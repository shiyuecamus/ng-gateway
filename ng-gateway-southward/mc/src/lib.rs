mod codec;
mod connector;
mod converter;
mod handle;
mod metadata;
mod protocol;
mod session;
mod typed_api;
mod types;

use connector::McConnector;
use converter::McConverter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "Melsec MC",
    description = "Mitsubishi PLC MC protocol driver",
    driver_type = "mc",
    component = McConnector,
    metadata_fn = build_metadata,
    model_convert = McConverter
);
