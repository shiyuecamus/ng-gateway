mod codec;
mod connector;
mod converter;
mod handle;
mod handler;
mod metadata;
mod session;
pub mod types;

pub use connector::Dnp3Connector;
use converter::Dnp3Converter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "DNP3",
    description = "DNP3 protocol driver",
    driver_type = "dnp3",
    component = Dnp3Connector,
    metadata_fn = build_metadata,
    model_convert = Dnp3Converter
);
