mod capacity;
pub mod codec;
pub mod driver;
pub mod factory;
mod metadata;
mod subscribe;
mod supervisor;
pub mod types;

pub use driver::OpcUaDriver;
pub use factory::OpcUaDriverFactory;
pub use types::{
    OpcUaAuth, OpcUaChannel, OpcUaChannelConfig, OpcUaDevice, OpcUaPoint, OpcUaReadMode,
    SecurityMode, SecurityPolicy,
};

use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "OPC UA",
    description = "OPC Unified Architecture industrial protocol driver",
    driver_type = "opcua",
    factory = OpcUaDriverFactory,
    metadata_fn = build_metadata
);
