mod config;
mod factory;
mod handlers;
mod metadata;
mod mqtt;
mod plugin;
mod provision;
mod supervisor;
mod topics;
mod types;

pub use config::{
    CommunicationConfig, ConnectionConfig, MessageFormat, ProvisionMethod, ThingsBoardPluginConfig,
};
use factory::ThingsBoardPluginFactory;
use metadata::build_metadata;
use ng_gateway_sdk::ng_plugin_factory;

// Export factory and static metadata via C ABI for loader
ng_plugin_factory!(
    name = "ThingsBoard",
    description = "ThingsBoard northward plugin",
    plugin_type = "thingsboard",
    factory = ThingsBoardPluginFactory,
    metadata_fn = build_metadata
);

// Connect to provision endpoint (no auth)
// Build a short, sanitized client_id to avoid broker rejection (BadClientId)
fn normalize_client_id<S: AsRef<str>>(input: S) -> String {
    const MAX_LEN: usize = 23;
    let filtered: String = input
        .as_ref()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if filtered.len() > MAX_LEN {
        filtered[..MAX_LEN].to_string()
    } else {
        filtered
    }
}
