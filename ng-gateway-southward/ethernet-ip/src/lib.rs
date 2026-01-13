pub mod codec;
pub mod driver;
pub mod factory;
pub mod metadata;
pub mod supervisor;
pub mod types;

use factory::EthernetIpDriverFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "Ethernet/IP",
    description = "Ethernet/IP industrial protocol driver for Allen-Bradley PLCs",
    driver_type = "ethernet-ip",
    factory = EthernetIpDriverFactory,
    metadata_fn = build_metadata
);
