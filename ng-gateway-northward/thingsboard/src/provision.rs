use crate::config::{ConnectionConfig, ProvisionMethod};
use ng_gateway_sdk::{ExtensionManager, ExtensionManagerExt, NorthwardError, NorthwardResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

/// Storage key for provision credentials in extension manager
///
/// This key is used to store and retrieve provision credentials
/// from the extension manager for persistent storage.
const STORAGE_KEY_PROVISION_CREDENTIALS: &str = "provision_credentials";

/// Device provision request for ThingsBoard
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionRequest {
    /// Device name
    pub device_name: String,
    /// Provision device key
    pub provision_device_key: String,
    /// Provision device secret
    pub provision_device_secret: String,
    /// Credentials type
    pub credentials_type: ProvisionMethod,
    /// Is Gateway
    #[serde(default = "ProvisionRequest::default_gateway")]
    pub gateway: bool,
    /// Access token (for ACCESS_TOKEN type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Username (for MQTT_BASIC type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Password (for MQTT_BASIC type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Client ID (for MQTT_BASIC type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Certificate hash (for X509_CERTIFICATE type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

impl ProvisionRequest {
    /// Create a new provision request
    pub fn new(
        device_name: String,
        provision_device_key: String,
        provision_device_secret: String,
        provision_method: ProvisionMethod,
    ) -> Self {
        Self {
            device_name,
            provision_device_key,
            provision_device_secret,
            credentials_type: provision_method,
            gateway: true,
            token: None,
            username: None,
            password: None,
            client_id: None,
            hash: None,
        }
    }

    /// Validate the provision request
    #[allow(unused)]
    pub fn validate(&self) -> NorthwardResult<()> {
        if self.device_name.is_empty() {
            return Err(NorthwardError::ConfigurationError {
                message: "Device name cannot be empty".to_string(),
            });
        }

        if self.provision_device_key.is_empty() {
            return Err(NorthwardError::ConfigurationError {
                message: "Provision device key cannot be empty".to_string(),
            });
        }

        if self.provision_device_secret.is_empty() {
            return Err(NorthwardError::ConfigurationError {
                message: "Provision device secret cannot be empty".to_string(),
            });
        }

        Ok(())
    }

    fn default_gateway() -> bool {
        true
    }
}

/// Device provision response from ThingsBoard
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionResponse {
    /// Device ID (for X509_CERTIFICATE type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Credentials type (only present on success)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials_type: Option<ProvisionMethod>,
    /// Credentials value (only present on success, format depends on type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials_value: Option<Value>,
    /// Status of the provision request
    pub status: ProvisionStatus,
    /// Error message (only present on failure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<String>,
}

/// Provision status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProvisionStatus {
    Success,
    Failure,
    NotFound,
}

/// MQTT Basic credentials structure for provision response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MqttBasicCredentials {
    pub client_id: String,
    pub user_name: String,
    pub password: String,
}

impl ProvisionResponse {
    /// Check if the provision was successful
    pub fn is_success(&self) -> bool {
        self.status == ProvisionStatus::Success
    }

    /// Extract credentials from the response
    pub fn extract_credentials(&self) -> NorthwardResult<ProvisionCredentials> {
        if !self.is_success() {
            let error_msg = self.error_msg.as_deref().unwrap_or("Unknown error");
            return Err(NorthwardError::ProvisionFailed {
                platform: "thingsboard".to_string(),
                reason: format!("Provision failed: {error_msg}"),
            });
        }

        let credentials_type =
            self.credentials_type
                .as_ref()
                .ok_or(NorthwardError::ProvisionFailed {
                    platform: "thingsboard".to_string(),
                    reason: "Missing credentials type in provision response".to_string(),
                })?;

        let credentials_value =
            self.credentials_value
                .as_ref()
                .ok_or(NorthwardError::ProvisionFailed {
                    platform: "thingsboard".to_string(),
                    reason: "Missing credentials value in provision response".to_string(),
                })?;

        match credentials_type {
            ProvisionMethod::AccessToken => {
                let token =
                    credentials_value
                        .as_str()
                        .ok_or(NorthwardError::DeserializationError {
                            reason: "Invalid access token in provision response".to_string(),
                        })?;

                Ok(ProvisionCredentials::AccessToken {
                    token: token.to_string(),
                })
            }
            ProvisionMethod::MqttBasic => {
                let creds: MqttBasicCredentials = serde_json::from_value(credentials_value.clone())
                    .map_err(|e| NorthwardError::DeserializationError {
                        reason: format!("Invalid MQTT basic credentials: {e}"),
                    })?;

                Ok(ProvisionCredentials::MqttBasic {
                    client_id: creds.client_id,
                    username: creds.user_name,
                    password: creds.password,
                })
            }
            ProvisionMethod::X509Certificate => {
                // Try to parse as object first (for certificate + key), then as string
                if let Some(cert_obj) = credentials_value.as_object() {
                    let cert = cert_obj.get("certificate").and_then(|v| v.as_str()).ok_or(
                        NorthwardError::DeserializationError {
                            reason: "Missing certificate in provision response".to_string(),
                        },
                    )?;

                    let private_key = cert_obj
                        .get("privateKey")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    Ok(ProvisionCredentials::X509Certificate {
                        certificate: cert.to_string(),
                        private_key,
                    })
                } else if let Some(cert) = credentials_value.as_str() {
                    Ok(ProvisionCredentials::X509Certificate {
                        certificate: cert.to_string(),
                        private_key: None,
                    })
                } else {
                    Err(NorthwardError::DeserializationError {
                        reason: "Invalid certificate in provision response".to_string(),
                    })
                }
            }
        }
    }
}

/// Extracted provision credentials (stored in extension manager)
/// Credentials for connecting to ThingsBoard
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProvisionCredentials {
    /// No authentication (insecure, for testing only)
    None,
    /// Access token authentication
    AccessToken { token: String },
    /// MQTT basic authentication (username/password)
    MqttBasic {
        client_id: String,
        username: String,
        password: String,
    },
    /// X.509 certificate authentication
    X509Certificate {
        certificate: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        private_key: Option<String>,
    },
}

/// Load existing credentials from extension manager or prepare for new provision
///
/// # Arguments
/// * `connection_config` - Connection configuration (to check if provision is needed)
/// * `extension_manager` - Extension manager for persistent storage
///
/// # Returns
/// * `Ok(Some(creds))` - Existing credentials found and loaded
/// * `Ok(None)` - No existing credentials, need to provision
/// * `Err(...)` - Failed to query extension manager
///
/// Returns:
/// - `Ok(Some(creds))` for direct connection modes (UsernamePassword, Token, X509)
/// - `Ok(None)` for Provision mode when credentials not found (need to provision)
/// - `Err(_)` for configuration or I/O errors
pub async fn load_or_prepare_credentials(
    connection_config: &ConnectionConfig,
    extension_manager: &Arc<dyn ExtensionManager>,
) -> NorthwardResult<Option<ProvisionCredentials>> {
    match connection_config {
        ConnectionConfig::None { .. } => {
            // No credentials needed for None mode
            Ok(Some(ProvisionCredentials::None))
        }
        ConnectionConfig::UsernamePassword {
            client_id,
            username,
            password,
            ..
        } => {
            // Direct connection with pre-configured credentials
            Ok(Some(ProvisionCredentials::MqttBasic {
                client_id: client_id.clone().unwrap_or_else(generate_client_id),
                username: username.clone(),
                password: password.clone(),
            }))
        }
        ConnectionConfig::Token { access_token, .. } => {
            // Direct connection with token
            Ok(Some(ProvisionCredentials::AccessToken {
                token: access_token.clone(),
            }))
        }
        ConnectionConfig::X509Certificate {
            cert_path,
            private_key_path,
            ..
        } => {
            // Direct connection with certificate
            // Read certificate files
            let certificate = tokio::fs::read_to_string(cert_path).await.map_err(|e| {
                NorthwardError::ConfigurationError {
                    message: format!("Failed to read certificate file: {}", e),
                }
            })?;

            let private_key = tokio::fs::read_to_string(private_key_path)
                .await
                .map_err(|e| NorthwardError::ConfigurationError {
                    message: format!("Failed to read private key file: {}", e),
                })?;

            Ok(Some(ProvisionCredentials::X509Certificate {
                certificate,
                private_key: Some(private_key),
            }))
        }
        ConnectionConfig::Provision { .. } => {
            // Provision mode - try to load from extension manager
            // Returns None if not found (caller should trigger provision)
            extension_manager
                .get::<ProvisionCredentials>(STORAGE_KEY_PROVISION_CREDENTIALS)
                .await
        }
    }
}

/// Store provision credentials to extension manager
///
/// # Arguments
/// * `credentials` - Credentials to store
/// * `extension_manager` - Extension manager for persistent storage
pub async fn store_credentials(
    credentials: &ProvisionCredentials,
    extension_manager: &Arc<dyn ExtensionManager>,
) -> NorthwardResult<()> {
    extension_manager
        .set(STORAGE_KEY_PROVISION_CREDENTIALS, credentials)
        .await
}

/// Generate a client ID for MQTT connection
///
/// Format: "ng-gateway-" + UUID (36 chars total, within MQTT 3.1.1 limit)
fn generate_client_id() -> String {
    format!("ng-gw-{}", Uuid::new_v4().simple())
}
