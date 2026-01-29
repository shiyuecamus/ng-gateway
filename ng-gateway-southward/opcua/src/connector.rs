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
    supervision::{Connector, Session, SessionContext},
    DriverError, DriverResult, FailureKind, FailurePhase, SouthwardInitContext,
};
use opcua::{
    client::{ClientBuilder, IdentityToken, Session as UaSession, SessionEventLoop},
    crypto::SecurityPolicy as UaSecurityPolicy,
    types::MessageSecurityMode,
};
use std::{sync::Arc, time::Duration};
use url::Url;

/// OPC UA connector built from `SouthwardInitContext` (no I/O).
#[derive(Clone)]
pub struct OpcUaConnector {
    channel: Arc<OpcUaChannel>,
    handle: Arc<OpcUaHandle>,
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
        cfg: &OpcUaChannelConfig,
    ) -> DriverResult<(Arc<UaSession>, SessionEventLoop)> {
        let mut client = Self::build_client(cfg)?.client().map_err(|e| {
            DriverError::SessionError(format!("OPC UA build client error: {:?}", e))
        })?;

        let identity: IdentityToken =
            cfg.auth.clone().try_into().map_err(|e| {
                DriverError::ConfigurationError(format!("OPC UA identity error: {e}"))
            })?;

        let url = cfg.url.trim();
        let endpoints = client
            .get_server_endpoints_from_url(url)
            .await
            .map_err(|err| {
                DriverError::SessionError(format!("OPC UA get endpoints error from {url}: {err}"))
            })?;

        let desired_policy = UaSecurityPolicy::from(cfg.security_policy);
        let desired_mode: MessageSecurityMode = cfg.security_mode.into();

        let mut selected = endpoints
            .into_iter()
            .find(|ep| {
                ep.security_mode == desired_mode
                    && UaSecurityPolicy::from_uri(ep.security_policy_uri.as_ref()) == desired_policy
            })
            .ok_or(DriverError::SessionError(format!(
                "No OPC UA endpoint matches desired security policy {:?} and mode {:?} for URL {}",
                desired_policy, desired_mode, url
            )))?;

        let original_endpoint_url = selected.endpoint_url.clone();
        if let (Ok(cfg_uri), Ok(mut ep_uri)) =
            (Url::parse(url), Url::parse(selected.endpoint_url.as_ref()))
        {
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
            "OPC UA selected endpoint for connection"
        );

        let (session, ev) = client
            .connect_to_endpoint_directly(selected, identity)
            .map_err(|e| DriverError::SessionError(format!("OPC UA connect-direct error: {e}")))?;

        Ok((session, ev))
    }
}

#[async_trait::async_trait]
impl Connector for OpcUaConnector {
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
        Ok(Self { channel, handle })
    }

    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let fut = Self::connect_once(&self.channel.config);
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
