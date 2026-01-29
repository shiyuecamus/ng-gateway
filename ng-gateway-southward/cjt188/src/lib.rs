pub mod codec;
mod connector;
mod converter;
mod handle;
mod metadata;
pub mod protocol;
mod session;
pub mod types;

pub use connector::Cjt188Connector;
use converter::Cjt188Converter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "CJ/T 188",
    description = "CJ/T 188 protocol driver",
    driver_type = "cjt188",
    component = Cjt188Connector,
    metadata_fn = build_metadata,
    model_convert = Cjt188Converter
);
