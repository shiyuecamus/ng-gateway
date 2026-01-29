pub mod config;
mod connector;
mod converter;
mod handle;
mod metadata;
mod session;

pub use config::PulsarPluginConfig;
use connector::PulsarConnector;
use converter::PulsarConverter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_plugin_factory;

// Export factory and static metadata via C ABI for loader
ng_plugin_factory!(
    name = "Pulsar",
    description = "Apache Pulsar northward plugin",
    plugin_type = "pulsar",
    component = PulsarConnector,
    metadata_fn = build_metadata,
    model_convert = PulsarConverter
);
