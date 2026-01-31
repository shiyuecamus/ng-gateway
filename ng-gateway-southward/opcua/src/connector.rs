//! OPC UA supervision connector.
//!
//! Final-form integration:
//! - `OpcUaConnector`: implements SDK `Connector` and is constructed via `Connector::new(ctx)`.
//! - `connect()`: establishes an OPC UA session and returns an attempt-scoped `OpcUaSession`.

use super::{
    handle::OpcUaHandle,
    session::OpcUaSession,
    types::OpcUaChannel,
    types::{OpcUaAuth, OpcUaChannelConfig, SecurityMode, SecurityPolicy},
};
use ng_gateway_sdk::{
    supervision::{Connector as NgConnector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, MeteredStream, SouthwardInitContext,
    SouthwardTransportMeter,
};
use opcua::{
    client::{
        transport::{
            Connector as UaConnector, OutgoingMessage, SecureChannelState, StreamConnection,
            StreamConnector, StreamTransport, TransportConfiguration,
        },
        ClientBuilder, ConnectionSource, IdentityToken, Session as UaSession, SessionEventLoop,
    },
    core::{
        comms::{tcp_codec::TcpCodec, url::hostname_port_from_url},
        constants::DEFAULT_OPC_UA_SERVER_PORT,
    },
    crypto::SecurityPolicy as UaSecurityPolicy,
    types::{
        DecodingOptions, EndpointDescription, Error as UaError, MessageSecurityMode, StatusCode,
        UserTokenType,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{ReadHalf, WriteHalf},
    net::TcpStream,
    sync::mpsc,
    time::{timeout, Duration as TokioDuration},
};
use tokio_util::codec::FramedRead;
use url::Url;

type MeteredReadHalf = ReadHalf<MeteredStream<TcpStream>>;
type MeteredWriteHalf = WriteHalf<MeteredStream<TcpStream>>;

fn endpoint_supports_identity(endpoint: &EndpointDescription, identity: &IdentityToken) -> bool {
    match identity {
        // This matches async-opcua's own selection semantics:
        // if a server does not advertise user identity tokens, we treat it as anonymous-friendly.
        IdentityToken::Anonymous => {
            endpoint.user_identity_tokens.is_none()
                || endpoint
                    .user_identity_tokens
                    .as_ref()
                    .is_some_and(|tokens| {
                        tokens
                            .iter()
                            .any(|p| p.token_type == UserTokenType::Anonymous)
                    })
        }
        IdentityToken::UserName(_, _) => endpoint
            .user_identity_tokens
            .as_ref()
            .is_some_and(|t| t.iter().any(|p| p.token_type == UserTokenType::UserName)),
        IdentityToken::X509(_, _) => endpoint
            .user_identity_tokens
            .as_ref()
            .is_some_and(|t| t.iter().any(|p| p.token_type == UserTokenType::Certificate)),
        IdentityToken::IssuedToken(_) => endpoint
            .user_identity_tokens
            .as_ref()
            .is_some_and(|t| t.iter().any(|p| p.token_type == UserTokenType::IssuedToken)),
    }
}

fn select_endpoint(
    endpoints: Vec<EndpointDescription>,
    desired_policy: UaSecurityPolicy,
    desired_mode: MessageSecurityMode,
    identity: &IdentityToken,
    configured_url: &str,
) -> DriverResult<EndpointDescription> {
    // Filter to compatible endpoints first.
    let mut candidates: Vec<EndpointDescription> = endpoints
        .into_iter()
        .filter(|ep| {
            ep.security_mode == desired_mode
                && UaSecurityPolicy::from_uri(ep.security_policy_uri.as_ref()) == desired_policy
        })
        .filter(|ep| endpoint_supports_identity(ep, identity))
        .collect();

    if candidates.is_empty() {
        return Err(DriverError::SessionError(format!(
            "No OPC UA endpoint matches desired security policy {:?} and mode {:?} (and identity token) for URL {}",
            desired_policy, desired_mode, configured_url
        )));
    }

    // Prefer the highest security level among compatible endpoints.
    // Tie-breaker: stable order by endpoint URL (helps deterministic selection across runs).
    candidates.sort_by(|a, b| {
        b.security_level
            .cmp(&a.security_level)
            .then_with(|| a.endpoint_url.as_ref().cmp(b.endpoint_url.as_ref()))
    });

    // We sorted by descending security level, ascending URL; take the first.
    let mut selected = candidates.remove(0);

    // Align host/port with configured URL, because some servers advertise non-routable endpoint URLs.
    let original_endpoint_url = selected.endpoint_url.clone();
    if let (Ok(cfg_uri), Ok(mut ep_uri)) = (
        Url::parse(configured_url.trim()),
        Url::parse(selected.endpoint_url.as_ref()),
    ) {
        if let Some(host) = cfg_uri.host_str() {
            let _ = ep_uri.set_host(Some(host));
        }
        if let Some(port) = cfg_uri.port() {
            let _ = ep_uri.set_port(Some(port));
        }
        selected.endpoint_url = ep_uri.to_string().into();
    }

    tracing::info!(
        endpoint_url = %selected.endpoint_url,
        original_endpoint_url = %original_endpoint_url,
        security_policy_uri = %selected.security_policy_uri,
        security_mode = ?selected.security_mode,
        security_level = selected.security_level,
        "OPC UA selected endpoint for connection"
    );

    Ok(selected)
}

async fn make_metered_stream_connection(
    endpoint_url: String,
    decoding_options: DecodingOptions,
    meter: Arc<dyn SouthwardTransportMeter>,
    connect_timeout_ms: u64,
) -> Result<StreamConnection<MeteredReadHalf, MeteredWriteHalf>, UaError> {
    let connect_timeout_ms = connect_timeout_ms.max(1);
    let connect_timeout = TokioDuration::from_millis(connect_timeout_ms);

    let connect_fut = async {
        let (host, port) = hostname_port_from_url(&endpoint_url, DEFAULT_OPC_UA_SERVER_PORT)
            .map_err(|e| UaError::new(e, "Failed to resolve URL to hostname and port"))?;

        let mut addrs = tokio::net::lookup_host(format!("{host}:{port}"))
            .await
            .map_err(|err| {
                UaError::new(
                    StatusCode::BadTcpEndpointUrlInvalid,
                    format!(
                        "Invalid address {}, cannot be parsed: {}",
                        endpoint_url, err
                    ),
                )
            })?;

        let addr = addrs.next().ok_or(UaError::new(
            StatusCode::BadTcpEndpointUrlInvalid,
            format!(
                "Invalid address {}, does not resolve to any socket",
                endpoint_url
            ),
        ))?;

        let socket = TcpStream::connect(addr).await.map_err(|err| {
            UaError::new(
                StatusCode::BadCommunicationError,
                format!("Could not connect to host {}, {}", addr, err),
            )
        })?;

        Ok::<TcpStream, UaError>(socket)
    };

    let socket = match timeout(connect_timeout, connect_fut).await {
        Ok(r) => r?,
        Err(_elapsed) => {
            return Err(UaError::new(
                StatusCode::BadTimeout,
                format!(
                    "TCP connect timeout after {}ms (endpoint_url={})",
                    connect_timeout_ms, endpoint_url
                ),
            ));
        }
    };

    let metered = MeteredStream::new(socket, meter);
    let (reader, writer) = tokio::io::split(metered);

    Ok(StreamConnection::new(
        FramedRead::new(reader, TcpCodec::new(decoding_options)),
        writer,
        endpoint_url,
    ))
}

/// TCP connector for OPC-UA that wraps the underlying stream with `MeteredStream`.
///
/// This implements the async-opcua client transport `Connector` trait so it can be used
/// for both:
/// - discovery calls (`GetEndpoints`, etc.)
/// - the actual session/event-loop connection
#[derive(Debug, Clone)]
pub(super) struct MeteredTcpConnector {
    endpoint_url: String,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    connect_timeout_ms: u64,
}

impl MeteredTcpConnector {
    #[inline]
    pub fn new(
        endpoint_url: String,
        transport_meter: Arc<dyn SouthwardTransportMeter>,
        connect_timeout_ms: u64,
    ) -> Self {
        Self {
            endpoint_url,
            transport_meter,
            connect_timeout_ms,
        }
    }
}

impl UaConnector for MeteredTcpConnector {
    type Transport = StreamTransport<MeteredReadHalf, MeteredWriteHalf>;

    async fn connect(
        &self,
        channel: Arc<SecureChannelState>,
        outgoing_recv: mpsc::Receiver<OutgoingMessage>,
        config: TransportConfiguration,
    ) -> Result<Self::Transport, StatusCode> {
        let meter = Arc::clone(&self.transport_meter);
        let connect_timeout_ms = self.connect_timeout_ms;
        let inner = StreamConnector::new(
            move |endpoint_url: String, decoding_options: DecodingOptions| {
                let meter = Arc::clone(&meter);
                async move {
                    make_metered_stream_connection(
                        endpoint_url,
                        decoding_options,
                        meter,
                        connect_timeout_ms,
                    )
                    .await
                }
            },
            self.endpoint_url.clone(),
        );
        inner.connect(channel, outgoing_recv, config).await
    }

    fn default_endpoint(&self) -> EndpointDescription {
        EndpointDescription::from(self.endpoint_url.as_str())
    }
}

#[derive(Debug, Clone)]
struct MeteredTcpConnectionSource {
    transport_meter: Arc<dyn SouthwardTransportMeter>,
    connect_timeout_ms: u64,
}

impl ConnectionSource for MeteredTcpConnectionSource {
    type Builder = MeteredTcpConnector;

    fn get_connector(&self, endpoint: &EndpointDescription) -> Result<Self::Builder, UaError> {
        Ok(MeteredTcpConnector::new(
            endpoint.endpoint_url.as_ref().to_string(),
            Arc::clone(&self.transport_meter),
            self.connect_timeout_ms,
        ))
    }
}

/// OPC UA connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct OpcUaConnector {
    channel: Arc<OpcUaChannel>,
    handle: Arc<OpcUaHandle>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
}

impl OpcUaConnector {
    #[inline]
    fn build_client(cfg: &OpcUaChannelConfig) -> DriverResult<ClientBuilder> {
        let requires_secure_channel = !matches!(cfg.security_policy, SecurityPolicy::None)
            && !matches!(cfg.security_mode, SecurityMode::None);
        let uses_x509_identity = matches!(cfg.auth, OpcUaAuth::Certificate { .. });
        let needs_app_cert = requires_secure_channel || uses_x509_identity;

        let mut builder = ClientBuilder::new()
            .application_name(&cfg.application_name)
            .application_uri(&cfg.application_uri)
            .pki_dir("./pki")
            .session_retry_limit(0)
            .session_timeout(cfg.session_timeout)
            .max_failed_keep_alive_count(cfg.max_failed_keep_alive_count as u64)
            .keep_alive_interval(Duration::from_millis(cfg.keep_alive_interval as u64));

        if needs_app_cert {
            builder = builder.trust_server_certs(true).create_sample_keypair(true);
        } else {
            builder = builder
                .trust_server_certs(false)
                .create_sample_keypair(true);
        }
        Ok(builder)
    }

    async fn connect_once(
        &self,
        cfg: &OpcUaChannelConfig,
    ) -> DriverResult<(Arc<UaSession>, SessionEventLoop<MeteredTcpConnector>)> {
        let client = Self::build_client(cfg)?.client().map_err(|e| {
            DriverError::SessionError(format!("OPC UA build client error: {:?}", e))
        })?;

        let identity: IdentityToken =
            cfg.auth.clone().try_into().map_err(|e| {
                DriverError::ConfigurationError(format!("OPC UA identity error: {e}"))
            })?;

        let url = cfg.url.trim();
        let connect_timeout_ms = self.channel.connection_policy.connect_timeout_ms.max(1);
        let discovery_connector = MeteredTcpConnector::new(
            url.to_string(),
            Arc::clone(&self.transport_meter),
            connect_timeout_ms,
        );
        let endpoints = client
            .get_endpoints(discovery_connector, &[], &[])
            .await
            .map_err(|err| {
                DriverError::SessionError(format!("OPC UA get endpoints error from {url}: {err}"))
            })?;

        let desired_policy = UaSecurityPolicy::from(cfg.security_policy);
        let desired_mode: MessageSecurityMode = cfg.security_mode.into();
        let selected = select_endpoint(endpoints, desired_policy, desired_mode, &identity, url)?;

        let connection_source = MeteredTcpConnectionSource {
            transport_meter: Arc::clone(&self.transport_meter),
            connect_timeout_ms,
        };
        let (session, ev) = client
            .session_builder()
            .with_connector(connection_source)
            .connect_to_endpoint_directly(selected)
            .map_err(|e| DriverError::SessionError(format!("OPC UA connect-direct error: {e}")))?
            .user_identity_token(identity)
            .build(client.certificate_store().clone())
            .map_err(|e| DriverError::SessionError(format!("OPC UA build session error: {e}")))?;

        Ok((session, ev))
    }
}

#[async_trait::async_trait]
impl NgConnector for OpcUaConnector {
    type InitContext = SouthwardInitContext;
    type Handle = OpcUaHandle;
    type Session = OpcUaSession;

    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let channel = Arc::clone(&ctx.runtime_channel)
            .downcast_arc::<OpcUaChannel>()
            .map_err(|_| DriverError::ConfigurationError("Invalid OpcUaChannel".to_string()))?;
        let handle = Arc::new(OpcUaHandle::new(
            Arc::clone(&channel),
            Arc::clone(&ctx.publisher),
            &ctx,
        ));
        Ok(Self {
            channel,
            handle,
            transport_meter: ctx.transport_meter,
        })
    }

    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let fut = self.connect_once(&self.channel.config);
        let (session, ev) = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            res = fut => res?,
        };
        Ok(OpcUaSession::new(Arc::clone(&self.handle), session, ev))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        _err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        FailureKind::Retryable
    }
}
