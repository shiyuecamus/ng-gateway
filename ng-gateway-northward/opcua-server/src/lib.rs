mod codec;
mod config;
mod connector;
mod converter;
mod handle;
mod metadata;
mod node_cache;
mod node_id;
mod queue;
mod server;
mod session;
mod write_dispatch;

pub use config::OpcuaServerPluginConfig;
use connector::OpcuaServerConnector;
use converter::OpcuaServerConverter;
use metadata::build_metadata;
use ng_gateway_sdk::ng_plugin_factory;

// Export factory and static metadata via C ABI for loader
ng_plugin_factory!(
    name = "OPC UA Server",
    description = "OPC UA server northward plugin",
    plugin_type = "opcua-server",
    component = OpcuaServerConnector,
    metadata_fn = build_metadata,
    model_convert = OpcuaServerConverter
);
