use super::{config::ThingsBoardPluginConfig, provision::ProvisionCredentials};
use crate::normalize_client_id;
use ng_gateway_sdk::{NorthwardError, NorthwardResult};
use rumqttc::{AsyncClient, EventLoop, MqttOptions, TlsConfiguration, Transport};
use std::time::Duration;
use uuid::Uuid;

/// Create MQTT client with credentials
///
/// This function encapsulates MQTT connection logic, including:
/// - Client creation with various authentication methods
/// - TLS configuration for X509 certificates
/// - Client ID generation
///
/// # Arguments
/// * `config` - ThingsBoard plugin configuration
/// * `credentials` - Provision credentials (token, username/password, X509, etc.)
///
/// # Returns
/// * `Ok((AsyncClient, EventLoop))` - Client and event loop ready for use
/// * `Err(...)` - Configuration or connection error
pub(super) fn connect_mqtt_client(
    config: &ThingsBoardPluginConfig,
    credentials: &ProvisionCredentials,
) -> NorthwardResult<(AsyncClient, EventLoop)> {
    // Build MQTT options
    let mut mqtt_options =
        MqttOptions::new(get_client_id(credentials), config.host(), config.port());

    // Set authentication based on credentials
    match credentials {
        ProvisionCredentials::None => {
            // No authentication required
        }
        ProvisionCredentials::AccessToken { token } => {
            mqtt_options.set_credentials(token, "");
        }
        ProvisionCredentials::MqttBasic {
            username, password, ..
        } => {
            mqtt_options.set_credentials(username, password);
        }
        ProvisionCredentials::X509Certificate {
            certificate,
            private_key,
        } => {
            // For X509, we need TLS configuration
            let cert_bytes = certificate.as_bytes().to_vec();
            let key_bytes = private_key.as_ref().map(|k| k.as_bytes().to_vec()).ok_or(
                NorthwardError::ConfigurationError {
                    message: "Private key is required for X509 authentication".to_string(),
                },
            )?;

            let tls_config = TlsConfiguration::Simple {
                ca: vec![], // TODO: Support CA certificate
                alpn: None,
                client_auth: Some((cert_bytes, key_bytes)),
            };

            mqtt_options.set_transport(Transport::Tls(tls_config));
        }
    }

    // Set MQTT options from communication config
    mqtt_options.set_keep_alive(Duration::from_secs(config.communication.keep_alive as u64));
    mqtt_options.set_clean_session(config.communication.clean_session);

    // Create client and event loop
    let (client, event_loop) = AsyncClient::new(mqtt_options, 100);

    Ok((client, event_loop))
}

/// Get client ID from credentials
///
/// # Arguments
/// * `credentials` - Provision credentials
///
/// # Returns
/// * Client ID string (from credentials or auto-generated)
#[inline]
fn get_client_id(credentials: &ProvisionCredentials) -> String {
    match credentials {
        ProvisionCredentials::MqttBasic { client_id, .. } => normalize_client_id(client_id),
        _ => {
            // Use a short 8-hex suffix to keep length small and compatible
            let short = Uuid::new_v4().simple().to_string();
            let short8 = &short[..8];
            normalize_client_id(format!("ng-gw-{}", short8))
        }
    }
}
