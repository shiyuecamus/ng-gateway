pub mod config;
mod factory;
mod metadata;
mod plugin;
mod supervisor;

pub use config::PulsarPluginConfig;
use factory::PulsarPluginFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_plugin_factory;

// Export factory and static metadata via C ABI for loader
ng_plugin_factory!(
    name = "Pulsar",
    description = "Apache Pulsar northward plugin",
    plugin_type = "pulsar",
    factory = PulsarPluginFactory,
    metadata_fn = build_metadata
);
