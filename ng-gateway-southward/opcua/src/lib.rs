mod codec;
mod driver;
mod factory;
mod metadata;
mod subscribe;
mod supervisor;
mod types;

use factory::OpcUaDriverFactory;
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
