//! OPC UA server runtime facade.
//!
//! We run an in-process OPC UA server (async-opcua-server) and expose a thin
//! handle to:
//! - reconcile + (re-)generate the application instance certificate before
//!   the server reads its PKI files, with a fully operator-controlled SAN list
//!   (see [`crate::pki`])
//! - bind a TCP listener on `bind_addr` (allowed to be wildcard) while the
//!   advertised endpoint URLs (`advertised_endpoints`) come from a separate
//!   field — this is what fixes KepServer's `Bad_TcpEndpointUrlInvalid` when
//!   the historical `host = "0.0.0.0"` leaked into endpoint discovery
//! - build / update AddressSpace (Objects/NG-Gateway/{channel}/{device}/{point})
//!   with full UTF-8 (CJK preserved) `NodeId`s, no string round-trip on the
//!   hot path
//! - dispatch OPC UA Write requests to gateway southward actions

use crate::{
    config::OpcuaServerPluginConfig, node_cache::NodeCache, pki::CertSummary,
    protocol::validate_advertised_endpoints, write_dispatch::WriteDispatcher,
};
use base64::Engine;
use ng_gateway_sdk::{
    log::fields as log_fields, AccessMode, DataType, NorthwardError, NorthwardResult,
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
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn, Instrument};

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

            // Dispatch to gateway directly with the typed `NodeId` — no
            // `to_string()` hot-path allocation; reverse-lookup uses the
            // NodeId's native `Hash + Eq`.
            let status = match self.write_dispatch.dispatch_write(&node_id, &variant).await {
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

/// Live runtime metadata produced once during `OpcuaServerRuntime::start`.
///
/// Captured here so `OpcuaServerSession` can publish it without re-deriving
/// any protocol-specific details.
#[derive(Debug, Clone)]
pub struct ServerRuntimeMetadata {
    /// Local socket bind address (verbatim from config).
    pub bind_addr: String,
    /// Validated advertised endpoint URLs (canonicalised).
    pub advertised_endpoints: Vec<String>,
    /// Live application instance certificate summary at runtime start.
    pub cert_summary: CertSummary,
}

#[derive(Clone)]
pub struct OpcuaServerRuntime {
    handle: opcua::server::ServerHandle,
    node_manager: Arc<NgGatewayNodeManager>,
    namespace_index: u16,
    root_id: NodeId,
    metadata: ServerRuntimeMetadata,
}

/// Inputs required by [`OpcuaServerRuntime::start`] to bind and bootstrap the OPC UA listener.
///
/// Grouped as a single parameter so callers can evolve the startup surface without
/// tripping readability or API limits (fewer positional arguments at callsites).
pub(crate) struct OpcuaServerRuntimeStartParams {
    /// Gateway-owned application identifier for diagnostics and tracing.
    pub(crate) app_id: i32,
    /// Canonical plugin configuration snapshot (immutable for the lifetime of the runtime).
    pub(crate) config: Arc<OpcuaServerPluginConfig>,
    /// Gateway northward runtime API (currently unused here; reserved for future hooks).
    pub(crate) runtime: Arc<dyn NorthwardRuntimeApi>,
    /// Live node-id cache backing address-space materialisation.
    pub(crate) node_cache: Arc<NodeCache>,
    /// Dispatcher that bridges OPC UA writes into gateway southward actions.
    pub(crate) write_dispatch: Arc<WriteDispatcher>,
    /// Fingerprint/metadata for the reconciled server certificate on disk.
    pub(crate) cert_summary: Arc<CertSummary>,
    /// Root directory housing server PKI material (trusted issuers/client certs).
    pub(crate) pki_dir: PathBuf,
    /// Cooperative shutdown signal propagated from the supervised northward lifecycle.
    pub(crate) shutdown: CancellationToken,
}

impl OpcuaServerRuntime {
    pub fn namespace_index(&self) -> u16 {
        self.namespace_index
    }

    pub fn metadata(&self) -> &ServerRuntimeMetadata {
        &self.metadata
    }

    /// Build, bind and spawn the OPC UA server task.
    ///
    /// # Lifecycle ownership
    /// PKI bootstrap (reconcile / generate / load / summary) **lives at
    /// connector scope**, not here. By the time this is called, the
    /// connector has already produced a valid `cert_summary` referring to
    /// the on-disk artefacts under `pki_dir`. This function therefore only
    /// performs cheap, deterministic work: validating endpoints, parsing
    /// the bind socket, materialising trusted client certs, building and
    /// binding the server. That keeps the connect-timeout budget tight even on
    /// weak hardware / debug builds where RSA keypair generation would
    /// otherwise dominate.
    pub async fn start(params: OpcuaServerRuntimeStartParams) -> NorthwardResult<Self> {
        let OpcuaServerRuntimeStartParams {
            app_id,
            config,
            runtime: _runtime,
            node_cache: _node_cache,
            write_dispatch,
            cert_summary,
            pki_dir,
            shutdown,
        } = params;

        let t0 = Instant::now();

        // Validate advertised endpoints up-front — cheaper to fail here than
        // halfway through PKI generation.
        let advertised = validate_advertised_endpoints(&config.advertised_endpoints)?;
        let primary = advertised[0].clone();

        // Parse bind address eagerly. Wildcards are explicitly allowed; the
        // advertised hostname is decoupled from this socket address.
        let bind_socket: SocketAddr =
            config
                .bind_addr
                .parse()
                .map_err(|e| NorthwardError::ConfigurationError {
                    message: format!(
                        "invalid bind_addr '{}': {e}; expected host:port (e.g. 0.0.0.0:4840)",
                        config.bind_addr
                    ),
                })?;

        // Root span for the whole runtime start sequence.
        //
        // IMPORTANT:
        // Some third-party crates may `tokio::spawn` during builder/setup phase
        // (e.g. inside `ServerBuilder.build()`), so we must enter this span
        // before any such calls to ensure `app_id` is inherited reliably.
        let runtime_span = tracing::info_span!(
            target: log_fields::TARGET_PLUGIN,
            "opcua-server-runtime",
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = i64::from(app_id)
        );
        let _enter = runtime_span.enter();

        info!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            bind_addr = %config.bind_addr,
            advertised_count = advertised.len(),
            primary_advertised = %primary.canonical(),
            namespace_uri = %config.namespace_uri,
            pki_dir = %pki_dir.display(),
            cert_thumbprint = %cert_summary.thumbprint_hex,
            "opcua-server runtime: building server"
        );

        // ---- Trusted client cert provisioning -----------------------------
        // Cheap, deterministic disk I/O (decode + write). The heavy PKI work
        // (RSA keypair generation, X509 self-sign) has already been done at
        // connector construction time — see `OpcuaServerConnector::from_init`.
        let t_pki = Instant::now();
        materialize_trusted_client_certs(&pki_dir, &config.trusted_client_certs)?;
        info!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            pki_prepare_ms = t_pki.elapsed().as_millis() as u64,
            trusted_client_certs = config.trusted_client_certs.len(),
            "opcua-server runtime: PKI prepared (reusing connector-owned cert)"
        );

        // ---- Server build -------------------------------------------------
        let endpoint_path = primary.path.as_str();
        let config_for_nm = Arc::clone(&config);
        let write_dispatch_for_nm = Arc::clone(&write_dispatch);
        let user_token_ids: &[&str] = &[ANONYMOUS_USER_TOKEN_ID];
        let builder = ServerBuilder::new()
            .application_name("NG-Gateway OPC UA Server")
            .application_uri(config.application_uri.clone())
            .product_uri(config.product_uri.clone())
            // PKI is now plugin-managed via `crate::pki`; we never let
            // async-opcua auto-generate because its SAN list is non-extensible.
            .create_sample_keypair(false)
            .certificate_path("own/cert.der")
            .private_key_path("private/private.pem")
            .pki_dir(pki_dir.clone())
            // `host()` and `port()` here only feed the advertised endpoint
            // URL composer in `info::base_endpoint()`; the actual TCP bind is
            // controlled by `Server::run_with(listener)` below.
            .host(primary.host.clone())
            .port(primary.port)
            .discovery_urls(config.advertised_endpoints.clone())
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
        // `async-opcua-server` uses `tokio::spawn` in a few sync helpers
        // (e.g. SyncSampler), so we must ensure we are inside *our* Tokio
        // runtime context here.
        let t_build = Instant::now();
        let (server, handle) = builder
            .build()
            .map_err(|e| NorthwardError::GatewayError { reason: e })?;
        info!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            build_ms = t_build.elapsed().as_millis() as u64,
            "opcua-server runtime: ServerBuilder.build completed"
        );

        // Find our node manager
        let node_manager = handle
            .node_managers()
            .get_of_type::<NgGatewayNodeManager>()
            .ok_or(NorthwardError::GatewayError {
                reason: "failed to locate NG-Gateway node manager".to_string(),
            })?;

        let namespace_index = handle
            .get_namespace_index(&config.namespace_uri)
            .unwrap_or(1);
        let root_id = NodeId::new(namespace_index, "NG-Gateway");

        // ---- Bind dedicated listener (decoupled from advertised hostname) -
        let t_bind = Instant::now();
        let listener =
            TcpListener::bind(bind_socket)
                .await
                .map_err(|e| NorthwardError::GatewayError {
                    reason: format!("failed to bind {bind_socket}: {e}"),
                })?;
        info!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            bind_addr = %bind_socket,
            bind_ms = t_bind.elapsed().as_millis() as u64,
            "opcua-server runtime: TCP listener bound"
        );

        // Run server in background using OUR listener so the bind address can
        // be a wildcard (multi-NIC / Docker) without polluting the endpoint URL.
        let server_span = tracing::info_span!(
            target: log_fields::TARGET_PLUGIN,
            "opcua-server-run",
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = i64::from(app_id)
        );
        tokio::spawn(
            async move {
                let _ = server.run_with(listener).await;
            }
            .instrument(server_span),
        );
        info!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            namespace_index = namespace_index,
            total_start_ms = t0.elapsed().as_millis() as u64,
            "opcua-server runtime: server task spawned"
        );

        let metadata = ServerRuntimeMetadata {
            bind_addr: config.bind_addr.clone(),
            advertised_endpoints: advertised.iter().map(|e| e.canonical()).collect(),
            // Reuse the connector-owned summary so all attempts/sessions
            // observe a stable, single source of truth for the live cert.
            cert_summary: cert_summary.as_ref().clone(),
        };

        Ok(Self {
            handle,
            node_manager,
            namespace_index,
            root_id,
            metadata,
        })
    }

    /// Create / update a `Variable` node for a given point.
    ///
    /// Accepts a typed `&NodeId` to avoid the `from_str` round-trip that the
    /// previous string-centric implementation paid on every materialisation.
    pub fn upsert_point_node(&self, meta: &PointMeta, node_id: &NodeId) {
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
        if !as_write.node_exists(node_id) {
            let dt = map_data_type(meta.data_type);
            let access = map_access_level(meta.access_mode);
            let mut vb =
                VariableBuilder::new(node_id, meta.point_key.as_ref(), meta.point_name.as_ref())
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

    /// Delete a previously-materialised variable node.
    pub fn remove_node(&self, node_id: &NodeId) {
        let mut as_write = self.node_manager.address_space().write();
        let _ = as_write.delete(node_id, true);
    }

    /// Update a variable node's value in the address space.
    ///
    /// Accepts a fully-formed `DataValue` so the caller can control
    /// `source_timestamp` / `server_timestamp` semantics.
    pub fn set_value(&self, node_id: &NodeId, dv: DataValue) {
        let _ = self
            .node_manager
            .set_value(self.handle.subscriptions(), node_id, None, dv);
    }
}

fn materialize_trusted_client_certs(pki_dir: &Path, certs: &[String]) -> NorthwardResult<()> {
    if certs.is_empty() {
        return Ok(());
    }

    let trusted_dir = pki_dir.join("trusted");
    fs::create_dir_all(&trusted_dir).map_err(|e| NorthwardError::ConfigurationError {
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
        fs::write(&path, &der).map_err(|e| NorthwardError::ConfigurationError {
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
