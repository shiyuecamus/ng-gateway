mod codec;
mod config;
mod factory;
mod metadata;
mod node_cache;
mod node_id;
mod plugin;
mod queue;
mod server;
mod write_dispatch;

pub use config::OpcuaServerPluginConfig;
use factory::OpcuaServerPluginFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_plugin_factory;

// Export factory and static metadata via C ABI for loader
ng_plugin_factory!(
    name = "OPC UA Server",
    description = "OPC UA server northward plugin",
    plugin_type = "opcua-server",
    factory = OpcuaServerPluginFactory,
    metadata_fn = build_metadata
);
