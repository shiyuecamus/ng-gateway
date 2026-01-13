use serde::{Deserialize, Serialize};

/// Enhanced gateway state with detailed status information
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GatewayState {
    Uninitialized,
    Initializing,
    Running,
    Pausing,
    Paused,
    Stopping,
    Stopped,
    Error { message: String, recoverable: bool },
    Maintenance,
}
