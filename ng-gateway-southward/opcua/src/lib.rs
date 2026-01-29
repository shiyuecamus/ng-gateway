mod capacity;
pub mod codec;
mod connector;
mod converter;
mod handle;
mod metadata;
mod session;
mod subscribe;
pub mod types;

pub use connector::OpcUaConnector;
pub use types::{
    OpcUaAuth, OpcUaChannel, OpcUaChannelConfig, OpcUaDevice, OpcUaPoint, OpcUaReadMode,
    SecurityMode, SecurityPolicy,
};

use converter::OpcUaConverter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

ng_driver_factory!(
    name = "OPC UA",
    description = "OPC Unified Architecture industrial protocol driver",
    driver_type = "opcua",
    component = OpcUaConnector,
    metadata_fn = build_metadata,
    model_convert = OpcUaConverter
);
