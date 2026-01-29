mod connector;
mod converter;
mod handle;
mod metadata;
pub mod protocol;
mod session;
mod types;

use connector::Iec104Connector;
use converter::Iec104Converter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_driver_factory;

// Export factory and static metadata via C ABI for loader
ng_driver_factory!(
    name = "IEC 60870-5-104",
    description = "IEC104 protocol driver",
    driver_type = "iec104",
    component = Iec104Connector,
    metadata_fn = build_metadata,
    model_convert = Iec104Converter,
    collect_max_inflight = 8
);
