pub mod codec;
mod connector;
mod converter;
mod handle;
pub mod metadata;
mod session;
pub mod types;

pub use connector::EthernetIpConnector;
use converter::EthernetIpConverter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "Ethernet/IP",
    description = "Ethernet/IP industrial protocol driver for Allen-Bradley PLCs",
    driver_type = "ethernet-ip",
    component = EthernetIpConnector,
    metadata_fn = build_metadata,
    model_convert = EthernetIpConverter
);
