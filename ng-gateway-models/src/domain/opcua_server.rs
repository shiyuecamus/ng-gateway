//! Domain types for OPC UA Server northward point export (Excel / future APIs).

use ng_gateway_sdk::northward::opcua_server::OpcuaServerRuntimeSnapshot;
use serde::{Deserialize, Serialize};

/// Aggregated inputs for rendering an OPC UA Server materialized-points export.
///
/// # Composition
/// - **Persistence metadata**: fields taken from [`crate::domain::app::AppInfo`]
///   and [`crate::domain::plugin::PluginInfo`] (name, type, version).
/// - **Live snapshot**: the `inspector:v1` capability payload
///   ([`OpcuaServerRuntimeSnapshot`]), where the plugin is the sole authority
///   for OPC UA wire-format strings inside [`OpcuaServerRuntimeSnapshot::materialized`].
///
/// Web handlers and other consumers assemble this struct after RBAC and
/// repository lookups, then pass it to a pure renderer (e.g. XLSX builder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpcuaServerExportContext {
    /// Northward app display name.
    pub app_name: String,
    /// Northward plugin type identifier (e.g. `opcua-server`).
    pub plugin_type: String,
    /// Northward plugin version string.
    pub plugin_version: String,
    /// Inspector snapshot (plugin-produced, self-contained rows).
    pub snapshot: OpcuaServerRuntimeSnapshot,
}
