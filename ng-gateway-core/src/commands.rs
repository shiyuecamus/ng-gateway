use crate::gateway::NGGateway;
use async_trait::async_trait;
use dashmap::DashMap;
use ng_gateway_error::NGResult;
use ng_gateway_models::core::metrics::GatewayStatus;
use ng_gateway_sdk::Command;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::sync::Arc;

/// Global command registry for gateway-level commands
pub static GATEWAY_COMMAND_REGISTRY: Lazy<DashMap<String, Arc<dyn GatewayCommandHandler>>> =
    Lazy::new(DashMap::new);

#[inline]
/// Get the command registry instance
pub fn gateway_command_registry() -> &'static DashMap<String, Arc<dyn GatewayCommandHandler>> {
    &GATEWAY_COMMAND_REGISTRY
}

/// Command handler interface for gateway-level commands
#[async_trait]
pub trait GatewayCommandHandler: Send + Sync + 'static {
    /// Handle a gateway-level command and return a JSON value on success.
    /// Implementers should be lightweight and avoid blocking operations.
    async fn handle(&self, cmd: &Command) -> NGResult<Value>;
}

/// Built-in handler for `get_gateway_status` (placeholder implementation)
pub struct GetStatusHandler {
    gateway: Arc<NGGateway>,
}

impl GetStatusHandler {
    /// Create a new handler bound to the gateway instance (weak reference)
    pub fn new(gateway: Arc<NGGateway>) -> Arc<Self> {
        Arc::new(Self { gateway })
    }
}

#[async_trait]
impl GatewayCommandHandler for GetStatusHandler {
    async fn handle(&self, _cmd: &Command) -> NGResult<Value> {
        let status: GatewayStatus = self.gateway.get_status().await;
        let snapshot = status.get_snapshot();
        let json = serde_json::to_value(&snapshot).unwrap();
        Ok(json)
    }
}
