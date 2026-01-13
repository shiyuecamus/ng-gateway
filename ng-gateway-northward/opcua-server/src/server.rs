//! OPC UA server runtime facade.
//!
//! We run an in-process OPC UA server (async-opcua-server) and expose a thin handle
//! to:
//! - build/update AddressSpace (Objects/NG-Gateway/{channel}/{device}/{point})
//! - write variable values efficiently
//! - dispatch OPC UA Write requests to gateway southward actions
use crate::{config::OpcuaServerPluginConfig, write_dispatch::WriteDispatcher};
use base64::Engine;
use ng_gateway_sdk::{
    AccessMode, DataType, NorthwardConnectionState, NorthwardError, NorthwardResult,
    NorthwardRuntimeApi, PointMeta,
};
use opcua::{
    crypto::SecurityPolicy,
    nodes::NodeType,
    server::{
        address_space::{AccessLevel, AddressSpace, ObjectBuilder, VariableBuilder},
        diagnostics::NamespaceMetadata,
        node_manager::{
            memory::{InMemoryNodeManager, InMemoryNodeManagerBuilder, InMemoryNodeManagerImpl},
            RequestContext, ServerContext, WriteNode,
        },
        ServerBuilder, ANONYMOUS_USER_TOKEN_ID,
    },
    sync::RwLock,
    types::{
        AttributeId, DataTypeId, DataValue, MessageSecurityMode, NodeId, ObjectId, StatusCode,
        Variant,
    },
};
use std::{net::IpAddr, path::PathBuf, str::FromStr, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::warn;

type NgGatewayNodeManager = InMemoryNodeManager<NgGatewayNodeManagerImpl>;

struct NgGatewayNodeManagerImpl {
    name: String,
    namespaces: Vec<NamespaceMetadata>,
    write_dispatch: Arc<WriteDispatcher>,
}

#[async_trait::async_trait]
impl InMemoryNodeManagerImpl for NgGatewayNodeManagerImpl {
    async fn init(&self, _address_space: &mut AddressSpace, _context: ServerContext) {}

    fn name(&self) -> &str {
        &self.name
    }

    fn namespaces(&self) -> Vec<NamespaceMetadata> {
        self.namespaces.clone()
    }

    async fn write(
        &self,
        context: &RequestContext,
        address_space: &RwLock<AddressSpace>,
        nodes_to_write: &mut [&mut WriteNode],
    ) -> Result<(), StatusCode> {
        for write in nodes_to_write.iter_mut() {
            let w = write.value();
            // Only allow writing Value attribute
            if w.attribute_id != AttributeId::Value {
                write.set_status(StatusCode::BadNotWritable);
                continue;
            }
            let node_id = w.node_id.clone();
            let Some(variant) = w.value.value.clone() else {
                write.set_status(StatusCode::BadNothingToDo);
                continue;
            };

            // Dispatch to gateway (no node_id string allocation on hot path)
            let status = match self
                .write_dispatch
                .dispatch_write(&node_id.to_string(), &variant)
                .await
            {
                Ok(()) => StatusCode::Good,
                Err(e) => {
                    warn!(node_id = %node_id, error = ?e, "Gateway write failed");
                    map_write_error(&e)
                }
            };
            write.set_status(status);

            // If accepted, update the stored value so reads/subscriptions reflect the write.
            if status.is_good() {
                let mut as_write = address_space.write();
                if let Some(NodeType::Variable(v)) = as_write.find_mut(&node_id) {
                    let dv = DataValue::new_now(variant);
                    v.set_data_value(dv.clone());
                    context
                        .subscriptions
                        .notify_data_change([(dv, &node_id, AttributeId::Value)].into_iter());
                }
            }
        }

        Ok(())
    }
}

fn map_write_error(err: &NorthwardError) -> StatusCode {
    match err {
        NorthwardError::NotFound { entity } => {
            // Entity-aware mapping for better client UX.
            if entity.starts_with("action:") {
                // Point exists but no corresponding southward action -> treat as not writable.
                StatusCode::BadNotWritable
            } else if entity.starts_with("device:") {
                StatusCode::BadNotConnected
            } else {
                // node_id / point / other
                StatusCode::BadNodeIdUnknown
            }
        }
        NorthwardError::NotConnected => StatusCode::BadNotConnected,
        NorthwardError::Timeout { .. } => StatusCode::BadTimeout,
        NorthwardError::ValidationFailed { reason } => {
            // We currently surface a few stable reason strings from WriteDispatcher.
            // Keep this mapping conservative and backward-compatible.
            let r = reason.as_str();
            if r.contains("not writeable") {
                StatusCode::BadUserAccessDenied
            } else if r.starts_with("type mismatch") {
                StatusCode::BadTypeMismatch
            } else if r.starts_with("out of range") {
                StatusCode::BadOutOfRange
            } else {
                StatusCode::BadInvalidArgument
            }
        }
        NorthwardError::GatewayError { reason } => {
            // Best-effort classification; core may return "channel X not connected"
            let r = reason.to_lowercase();
            if r.contains("not connected") || r.contains("disconnected") {
                StatusCode::BadNotConnected
            } else if r.contains("timeout") {
                StatusCode::BadTimeout
            } else {
                StatusCode::BadInternalError
            }
        }
        _ => StatusCode::BadInternalError,
    }
}

#[derive(Clone)]
pub struct OpcuaServerRuntime {
    handle: opcua::server::ServerHandle,
    node_manager: Arc<NgGatewayNodeManager>,
    namespace_index: u16,
    root_id: NodeId,
}

impl OpcuaServerRuntime {
    pub fn namespace_index(&self) -> u16 {
        self.namespace_index
    }

    pub async fn start(
        plugin_id: i32,
        config: Arc<OpcuaServerPluginConfig>,
        _runtime: Arc<dyn NorthwardRuntimeApi>,
        _node_cache: Arc<crate::node_cache::NodeCache>,
        write_dispatch: Arc<WriteDispatcher>,
        conn_state_tx: watch::Sender<NorthwardConnectionState>,
        shutdown: CancellationToken,
    ) -> NorthwardResult<Self> {
        if config.port == 0 {
            return Err(NorthwardError::ConfigurationError {
                message: "invalid port: must be in range 1..=65535".to_string(),
            });
        }

        // Build server
        // PKI directory is not user-configurable by design:
        // use a stable, per-installed-plugin layout.
        let pki_dir = format!("pki/plugin/{plugin_id}");

        // Materialize configured trusted client certificates into PKI trust store
        // so native `CertificateStore` validation can pick them up.
        materialize_trusted_client_certs(&pki_dir, &config.trusted_client_certs)?;

        let endpoint_path = "/";
        let config_for_nm = Arc::clone(&config);
        let write_dispatch_for_nm = Arc::clone(&write_dispatch);
        let user_token_ids: &[&str] = &[ANONYMOUS_USER_TOKEN_ID];
        let builder = ServerBuilder::new()
            .application_name("NG-Gateway OPC UA Server")
            .application_uri(config.application_uri.clone())
            .product_uri(config.product_uri.clone())
            // Production note: this makes first-run easier by generating a self-signed cert
            // into the PKI dir when missing. You can turn this off and provide your own certs
            // by pre-provisioning files under `pki/plugin/{plugin_id}`.
            .create_sample_keypair(true)
            .certificate_path("own/cert.der")
            .private_key_path("private/private.pem")
            .pki_dir(pki_dir)
            .host(config.host.clone())
            .port(config.port)
            .discovery_urls(default_discovery_urls(
                &config.host,
                config.port,
                endpoint_path,
            ))
            .add_endpoint(
                "no_security",
                (
                    endpoint_path,
                    SecurityPolicy::None,
                    MessageSecurityMode::None,
                    user_token_ids,
                ),
            )
            .add_endpoint(
                "basic256sha256_sign_encrypt",
                (
                    endpoint_path,
                    SecurityPolicy::Basic256Sha256,
                    MessageSecurityMode::SignAndEncrypt,
                    user_token_ids,
                ),
            )
            .default_endpoint("no_security")
            .with_node_manager(InMemoryNodeManagerBuilder::new(
                move |context: ServerContext, address_space: &mut AddressSpace| {
                    // Ensure our namespace is registered in both type tree and address space
                    let namespace_index = {
                        let mut type_tree = context.type_tree.write();
                        type_tree
                            .namespaces_mut()
                            .add_namespace(config_for_nm.namespace_uri.as_str())
                    };
                    address_space.add_namespace(&config_for_nm.namespace_uri, namespace_index);

                    // Create the root object: Objects/NG-Gateway
                    let root_id = NodeId::new(namespace_index, "NG-Gateway");
                    let _ = ObjectBuilder::new(&root_id, "NG-Gateway", "NG-Gateway")
                        .organized_by(ObjectId::ObjectsFolder)
                        .insert(address_space);

                    NgGatewayNodeManagerImpl {
                        name: "ng-gateway".to_string(),
                        namespaces: vec![NamespaceMetadata {
                            namespace_uri: config_for_nm.namespace_uri.clone(),
                            namespace_index,
                            ..Default::default()
                        }],
                        write_dispatch: Arc::clone(&write_dispatch_for_nm),
                    }
                },
            ))
            .token(shutdown.clone());

        // IMPORTANT:
        // `async-opcua-server` uses `tokio::spawn` in a few sync helpers (e.g. SyncSampler),
        // so we must ensure we are inside *our* Tokio runtime context here.
        let (server, handle) = builder
            .build()
            .map_err(|e| NorthwardError::GatewayError { reason: e })?;

        // Find our node manager
        let node_manager = handle
            .node_managers()
            .get_of_type::<NgGatewayNodeManager>()
            .ok_or_else(|| NorthwardError::GatewayError {
                reason: "failed to locate NG-Gateway node manager".to_string(),
            })?;

        let namespace_index = handle
            .get_namespace_index(&config.namespace_uri)
            .unwrap_or(1);
        // Intentionally no info/debug logs here (can be very chatty in production).
        let root_id = NodeId::new(namespace_index, "NG-Gateway");

        // Run server in background
        tokio::spawn(async move {
            let _ = server.run().await;
        });

        let _ = conn_state_tx.send(NorthwardConnectionState::Connected);

        Ok(Self {
            handle,
            node_manager,
            namespace_index,
            root_id,
        })
    }

    pub fn upsert_point_node(&self, meta: &PointMeta, node_id: &str) {
        let node_id = match NodeId::from_str(node_id) {
            Ok(v) => v,
            Err(_) => return,
        };
        let channel_obj = NodeId::new(
            self.namespace_index,
            format!("ch.{}", meta.channel_name.as_ref()),
        );
        let device_obj = NodeId::new(
            self.namespace_index,
            format!(
                "ch.{}.dev.{}",
                meta.channel_name.as_ref(),
                meta.device_name.as_ref()
            ),
        );

        let mut as_write = self.node_manager.address_space().write();

        // Ensure hierarchy objects exist
        if !as_write.node_exists(&self.root_id) {
            let _ = ObjectBuilder::new(&self.root_id, "NG-Gateway", "NG-Gateway")
                .organized_by(ObjectId::ObjectsFolder)
                .insert(&mut *as_write);
        }
        if !as_write.node_exists(&channel_obj) {
            let browse = meta.channel_name.as_ref();
            let _ = ObjectBuilder::new(&channel_obj, browse, browse)
                .organized_by(self.root_id.clone())
                .insert(&mut *as_write);
        }
        if !as_write.node_exists(&device_obj) {
            let browse = meta.device_name.as_ref();
            let _ = ObjectBuilder::new(&device_obj, browse, browse)
                .organized_by(channel_obj.clone())
                .insert(&mut *as_write);
        }

        // Create variable if missing
        if !as_write.node_exists(&node_id) {
            let dt = map_data_type(meta.data_type);
            let access = map_access_level(meta.access_mode);
            let mut vb =
                VariableBuilder::new(&node_id, meta.point_key.as_ref(), meta.point_name.as_ref())
                    .data_type(dt)
                    .value(Variant::Empty)
                    .access_level(access)
                    .user_access_level(access)
                    .organized_by(device_obj.clone());
            if let Some(desc) = meta.description.as_ref() {
                vb = vb.description(desc.as_ref());
            }
            let _ = vb.insert(&mut *as_write);
        }
    }

    pub fn remove_node(&self, node_id: &str) {
        let Ok(id) = NodeId::from_str(node_id) else {
            return;
        };
        let mut as_write = self.node_manager.address_space().write();
        let _ = as_write.delete(&id, true);
    }

    pub fn set_value(&self, node_id: &str, value: Variant) {
        let Ok(id) = NodeId::from_str(node_id) else {
            return;
        };
        let dv = DataValue::new_now(value);
        let _ = self
            .node_manager
            .set_value(self.handle.subscriptions(), &id, None, dv);
    }
}

fn materialize_trusted_client_certs(pki_dir: &str, certs: &[String]) -> NorthwardResult<()> {
    if certs.is_empty() {
        return Ok(());
    }

    let trusted_dir = PathBuf::from(pki_dir).join("trusted");
    std::fs::create_dir_all(&trusted_dir).map_err(|e| NorthwardError::ConfigurationError {
        message: format!(
            "failed to create PKI trusted directory {}: {e}",
            trusted_dir.display()
        ),
    })?;

    let decoded =
        decode_cert_inputs_to_der(certs).map_err(|reason| NorthwardError::ConfigurationError {
            message: format!("invalid trusted_client_certs: {reason}"),
        })?;

    for der in decoded {
        let x509 = opcua::crypto::X509::from_der(&der).map_err(|e| {
            NorthwardError::ConfigurationError {
                message: format!("invalid certificate DER: {e}"),
            }
        })?;
        let file_name = opcua::crypto::CertificateStore::cert_file_name(&x509);
        let path = trusted_dir.join(file_name);
        std::fs::write(&path, &der).map_err(|e| NorthwardError::ConfigurationError {
            message: format!(
                "failed to write trusted certificate {}: {e}",
                path.display()
            ),
        })?;
    }

    Ok(())
}

fn decode_cert_inputs_to_der(inputs: &[String]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    for (idx, raw) in inputs.iter().enumerate() {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }

        if s.contains("-----BEGIN CERTIFICATE-----") {
            let before_len = out.len();
            let mut in_block = false;
            let mut b64 = String::new();
            for line in s.lines() {
                let line = line.trim();
                if line == "-----BEGIN CERTIFICATE-----" {
                    in_block = true;
                    b64.clear();
                    continue;
                }
                if line == "-----END CERTIFICATE-----" {
                    if !b64.is_empty() {
                        let bytes = decode_base64_stripped(&b64)
                            .map_err(|e| format!("cert[{idx}] PEM base64 decode failed: {e}"))?;
                        out.push(bytes);
                    }
                    in_block = false;
                    b64.clear();
                    continue;
                }
                if in_block {
                    b64.push_str(line);
                }
            }
            // If marker was present but we didn't decode anything, treat as error.
            if out.len() == before_len {
                return Err(format!(
                    "cert[{idx}] contains PEM marker but no valid CERTIFICATE block"
                ));
            }
        } else {
            let bytes = decode_base64_stripped(s)
                .map_err(|e| format!("cert[{idx}] base64 DER decode failed: {e}"))?;
            out.push(bytes);
        }
    }
    Ok(out)
}

fn decode_base64_stripped(s: &str) -> Result<Vec<u8>, String> {
    let mut compact = String::with_capacity(s.len());
    for ch in s.chars() {
        if !ch.is_whitespace() {
            compact.push(ch);
        }
    }
    base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|e| e.to_string())
}

fn map_access_level(mode: AccessMode) -> AccessLevel {
    match mode {
        AccessMode::Read => AccessLevel::CURRENT_READ,
        AccessMode::Write => AccessLevel::CURRENT_WRITE,
        AccessMode::ReadWrite => AccessLevel::CURRENT_READ | AccessLevel::CURRENT_WRITE,
    }
}

fn map_data_type(dt: DataType) -> DataTypeId {
    match dt {
        DataType::Boolean => DataTypeId::Boolean,
        DataType::Int8 => DataTypeId::SByte,
        DataType::UInt8 => DataTypeId::Byte,
        DataType::Int16 => DataTypeId::Int16,
        DataType::UInt16 => DataTypeId::UInt16,
        DataType::Int32 => DataTypeId::Int32,
        DataType::UInt32 => DataTypeId::UInt32,
        DataType::Int64 => DataTypeId::Int64,
        DataType::UInt64 => DataTypeId::UInt64,
        DataType::Float32 => DataTypeId::Float,
        DataType::Float64 => DataTypeId::Double,
        DataType::String => DataTypeId::String,
        DataType::Binary => DataTypeId::ByteString,
        DataType::Timestamp => DataTypeId::DateTime,
    }
}

fn default_discovery_urls(host: &str, port: u16, endpoint_path: &str) -> Vec<String> {
    // `async-opcua-server` requires discovery_urls to be non-empty.
    // For bind-all addresses, advertise loopback by default (clients cannot connect to 0.0.0.0).
    let is_wildcard = matches!(host, "0.0.0.0" | "::" | "0:0:0:0:0:0:0:0");
    let path = if endpoint_path.starts_with('/') {
        endpoint_path
    } else {
        "/"
    };

    if is_wildcard {
        return vec![
            format!("opc.tcp://localhost:{port}{path}"),
            format!("opc.tcp://127.0.0.1:{port}{path}"),
            format!("opc.tcp://[::1]:{port}{path}"),
        ];
    }

    // If host is an IPv6 literal, make sure it is bracketed for URL authority.
    let host_for_url = match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]"),
        _ => host.to_string(),
    };

    vec![format!("opc.tcp://{host_for_url}:{port}{path}")]
}

#[cfg(test)]
mod tests {
    use super::default_discovery_urls;

    #[test]
    fn default_discovery_urls_is_non_empty_for_wildcard_hosts() {
        let urls = default_discovery_urls("0.0.0.0", 4840, "/");
        assert!(!urls.is_empty());
        assert!(urls.iter().any(|u| u.contains("localhost:4840")));
    }

    #[test]
    fn default_discovery_urls_uses_given_host_for_normal_host() {
        let urls = default_discovery_urls("192.168.1.10", 4840, "/");
        assert_eq!(urls, vec!["opc.tcp://192.168.1.10:4840/".to_string()]);
    }

    #[test]
    fn default_discovery_urls_brackets_ipv6() {
        let urls = default_discovery_urls("::1", 4840, "/");
        // NOTE: ::1 is not treated as wildcard; it should be directly usable by clients.
        assert_eq!(urls, vec!["opc.tcp://[::1]:4840/".to_string()]);
    }
}
