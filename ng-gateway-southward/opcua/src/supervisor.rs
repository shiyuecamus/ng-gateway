use super::types::{
    OpcUaAuth, OpcUaChannelConfig, SecurityMode as NgSecurityMode,
    SecurityPolicy as NgSecurityPolicy,
};
use crate::types::OpcUaChannel;
use arc_swap::ArcSwapOption;
use backoff::{self, backoff::Backoff, ExponentialBackoff};
use futures::{pin_mut, StreamExt};
use ng_gateway_sdk::{
    build_exponential_backoff, DriverError, DriverResult, SouthwardConnectionState,
};
use opcua::{
    client::{
        ClientBuilder, IdentityToken, Session, SessionActivity, SessionEventLoop, SessionPollResult,
    },
    crypto::SecurityPolicy as UaSecurityPolicy,
    types::MessageSecurityMode,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use url::Url;

pub(super) type SharedSession = Arc<SessionEntry>;
pub(super) type OnConnectedCallback =
    Box<dyn Fn(Arc<Session>) -> JoinHandle<()> + Send + Sync + 'static>;

/// Shared session entry guarded by ArcSwapOption for lock-free hot-path reads.
/// Supervisor task owns the lifecycle and reconnect loop.
pub(super) struct SessionEntry {
    pub session: ArcSwapOption<Session>,
    pub healthy: AtomicBool,
    pub shutdown: AtomicBool,
    pub last_error: std::sync::Mutex<Option<String>>,
}

impl SessionEntry {
    #[inline]
    pub fn new_empty() -> Self {
        Self {
            session: ArcSwapOption::from(None),
            healthy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_error: std::sync::Mutex::new(None),
        }
    }
}

pub(super) struct SessionSupervisor {
    pub shared: SharedSession,
    pub cancel_token: CancellationToken,
    pub state_tx: watch::Sender<SouthwardConnectionState>,
}

impl SessionSupervisor {
    #[inline]
    pub fn new(
        shared: SharedSession,
        cancel_token: CancellationToken,
        state_tx: watch::Sender<SouthwardConnectionState>,
    ) -> Self {
        Self {
            shared,
            cancel_token,
            state_tx,
        }
    }

    #[inline]
    fn build_client(cfg: &OpcUaChannelConfig) -> DriverResult<ClientBuilder> {
        // Decide whether we need an application instance certificate based on:
        // - Security policy / mode (any policy other than None with Sign / SignAndEncrypt)
        // - X509-based user authentication
        let requires_secure_channel = !matches!(cfg.security_policy, NgSecurityPolicy::None)
            && !matches!(cfg.security_mode, NgSecurityMode::None);
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
            // For encrypted endpoints or X509 auth, enable automatic application instance
            // certificate generation and trust unknown server certs by default. This trades
            // some security strictness for easier out-of-the-box connectivity, similar to
            // typical UA client defaults. Operators can later manage PKI folders explicitly
            // if stricter trust is required.
            builder = builder.trust_server_certs(true).create_sample_keypair(true);
        } else {
            // For SecurityPolicy=None with non-certificate auth, no application certificate
            // is needed. Disabling sample keypair generation avoids unnecessary certificate
            // I/O and noisy "Invalid der" errors when PKI is not configured.
            builder = builder
                .trust_server_certs(false)
                .create_sample_keypair(false);
        }
        Ok(builder)
    }

    async fn connect_once(
        cfg: &OpcUaChannelConfig,
    ) -> DriverResult<(Arc<Session>, SessionEventLoop)> {
        let mut client = Self::build_client(cfg)?.client().map_err(|e| {
            DriverError::SessionError(format!("OPC UA build client error: {:?}", e))
        })?;

        let identity: IdentityToken =
            cfg.auth.clone().try_into().map_err(|e| {
                DriverError::ConfigurationError(format!("OPC UA identity error: {e}"))
            })?;

        // Use trimmed URL to avoid subtle parsing issues (whitespace, invisible characters).
        let url = cfg.url.trim();

        // Discover endpoints from the discovery URL so we can implement a more tolerant
        // Prosys-like matching strategy that does not require the configured URL path to
        // exactly match the server's advertised endpoint URL.
        let endpoints = match client.get_server_endpoints_from_url(url).await {
            Ok(endpoints) => {
                for ep in &endpoints {
                    tracing::info!(
                        endpoint_url = %ep.endpoint_url,
                        security_policy_uri = %ep.security_policy_uri,
                        security_mode = ?ep.security_mode,
                        "OPC UA discovered endpoint"
                    );
                }
                endpoints
            }
            Err(err) => {
                return Err(DriverError::SessionError(format!(
                    "OPC UA get endpoints error from {}: {err}",
                    url
                )));
            }
        };

        // Select an endpoint that matches the requested security policy and mode.
        let desired_policy = UaSecurityPolicy::from(cfg.security_policy);
        let desired_mode: MessageSecurityMode = cfg.security_mode.into();

        let mut selected = endpoints
            .into_iter()
            .find(|ep| {
                ep.security_mode == desired_mode
                    && UaSecurityPolicy::from_uri(ep.security_policy_uri.as_ref()) == desired_policy
            })
            .ok_or_else(|| {
                DriverError::SessionError(format!(
                    "No OPC UA endpoint matches desired security policy {:?} and mode {:?} for URL {}",
                    desired_policy, desired_mode, url
                ))
            })?;

        // In many real-world deployments (including popular demo servers), the server will
        // advertise an endpoint URL whose hostname is not directly reachable from the client
        // (e.g. a local machine name that resolves to an IPv6 link-local address, while the
        // server is actually only listening on IPv4 127.0.0.1). If we connect to that URL
        // verbatim, the TCP connection can fail with ConnectionRefused even though the
        // discovery URL worked.
        //
        // To make the client more robust and user-friendly, we override the selected
        // endpoint's host (and port, when explicitly set) with the host/port from the
        // configured discovery URL. This behaves similarly to common OPC UA clients like
        // Prosys/KepServer which effectively treat the configured URL as authoritative for
        // transport, while still honoring the server-advertised security policy/mode.
        let original_endpoint_url = selected.endpoint_url.clone();
        if let (Ok(cfg_uri), Ok(mut ep_uri)) =
            (Url::parse(url), Url::parse(selected.endpoint_url.as_ref()))
        {
            if let Some(host) = cfg_uri.host_str() {
                if let Err(err) = ep_uri.set_host(Some(host)) {
                    tracing::debug!(
                        error = ?err,
                        "Failed to override OPC UA endpoint host; falling back to server advertised host"
                    );
                }
            }

            // If the configuration explicitly specifies a port, prefer it; otherwise keep
            // whatever the server advertised (usually the same as discovery).
            if let Some(port) = cfg_uri.port() {
                if let Err(err) = ep_uri.set_port(Some(port)) {
                    tracing::debug!(
                        error = ?err,
                        "Failed to override OPC UA endpoint port; falling back to server advertised port"
                    );
                }
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

        // Connect directly to the selected endpoint. This bypasses the stricter
        // `connect_to_matching_endpoint` URL path comparison and behaves more like
        // Prosys / KepServer, which accept the server's advertised endpoint path.
        let (session, ev) = client
            .connect_to_endpoint_directly(selected, identity)
            .map_err(|e| DriverError::SessionError(format!("OPC UA connect-direct error: {e}")))?;

        Ok((session, ev))
    }

    #[inline]
    /// Record last error message into shared state.
    fn set_last_error(shared: &SharedSession, msg: impl Into<String>) {
        let _ = shared.last_error.lock().map(|mut e| *e = Some(msg.into()));
    }

    pub async fn run(self, channel: Arc<OpcUaChannel>, on_connected: Option<OnConnectedCallback>) {
        let shared = Arc::clone(&self.shared);
        let cancel = self.cancel_token.clone();
        let state_tx = self.state_tx.clone();

        // Reset shutdown flag at the beginning of a supervisor lifecycle
        shared.shutdown.store(false, Ordering::Release);

        tokio::spawn(async move {
            // Supervisor outer loop with reconnect/backoff, aligned with IEC104 design
            let mut bo: ExponentialBackoff =
                build_exponential_backoff(&channel.connection_policy.backoff);
            let mut attempt: u32 = 0;
            let mut on_connected_opt = on_connected;

            loop {
                let _ = state_tx.send(SouthwardConnectionState::Connecting);

                // Attempt connection with backoff until success or cancellation
                let (session, ev) = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        let _ = state_tx
                            .send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        return;
                    }
                    match Self::connect_once(&channel.config).await {
                        Ok(pair) => break pair,
                        Err(e) => {
                            attempt = attempt.saturating_add(1);
                            let delay = bo.next_backoff().unwrap_or_else(|| {
                                Duration::from_millis(
                                    channel.connection_policy.backoff.max_interval_ms,
                                )
                            });
                            tracing::warn!(attempt = attempt, delay_ms = delay.as_millis() as u64, error = %e, "OPC UA connect retry");
                            tokio::select! {
                                _ = cancel.cancelled() => {
                                    shared.shutdown.store(true, Ordering::Release);
                                    let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                                    return;
                                }
                                _ = tokio::time::sleep(delay) => {}
                            }
                        }
                    }
                };

                // Run one attempt's event loop and wait for completion or cancellation
                let child = cancel.child_token();
                let mut task = tokio::spawn(Self::run_event_loop(
                    Arc::clone(&shared),
                    Arc::clone(&session),
                    ev,
                    child.clone(),
                    on_connected_opt.take(),
                    state_tx.clone(),
                ));

                tokio::select! {
                    _ = cancel.cancelled() => {
                        child.cancel();
                        let _ = task.await;
                        shared.session.store(None);
                        shared.healthy.store(false, Ordering::Release);
                        shared.shutdown.store(true, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        Self::set_last_error(&shared, "supervisor cancelled");
                        break;
                    }
                    res = &mut task => {
                        // attempt finished, clear state then backoff and retry
                        shared.session.store(None);
                        shared.healthy.store(false, Ordering::Release);
                        let seen_active = res.unwrap_or(false);
                        if seen_active {
                            bo.reset();
                            attempt = 0;
                        }

                        match bo.next_backoff() {
                            Some(dur) => {
                                attempt = attempt.saturating_add(1);
                                tracing::warn!(attempt = attempt, delay_ms = dur.as_millis() as u64, "OPC UA connect retry");
                                tokio::select! {
                                    _ = cancel.cancelled() => { break; }
                                    _ = tokio::time::sleep(dur) => {
                                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                                    }
                                }
                            }
                            None => {
                                let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Drive the OPC UA SessionEventLoop until cancelled or the stream ends.
    /// Updates shared health flags as events arrive.
    async fn run_event_loop(
        shared: SharedSession,
        session: Arc<Session>,
        ev: SessionEventLoop,
        cancel: CancellationToken,
        mut on_connected: Option<OnConnectedCallback>,
        state_tx: watch::Sender<SouthwardConnectionState>,
    ) -> bool {
        let stream = ev.enter();
        pin_mut!(stream);
        let mut seen_active = false;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    // Proactively disable reconnects and close the session with a short timeout.
                    session.disable_reconnects();
                    let disconnect_session = Arc::clone(&session);
                    let _ = tokio::time::timeout(Duration::from_secs(2), disconnect_session.disconnect())
                    .await;
                    shared.healthy.store(false, Ordering::Release);
                    let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                    return seen_active;
                }
                maybe_item = stream.next() => {
                    match maybe_item {
                        Some(Ok(ev)) => {
                            match ev {
                                SessionPollResult::Reconnected(_) | SessionPollResult::Transport(_) => {
                                    shared.healthy.store(true, Ordering::Release);
                                    let _ = state_tx.send(SouthwardConnectionState::Connected);
                                    if !seen_active {
                                        seen_active = true;
                                        shared.session.store(Some(session.clone()));
                                        if let Some(f) = &mut on_connected {
                                            // Spawn user callback once on first connect
                                            drop(f(session.clone()));
                                        }
                                    }
                                }
                                SessionPollResult::SessionActivity(act) => {
                                    match act {
                                        SessionActivity::KeepAliveSucceeded => {
                                            shared.healthy.store(true, Ordering::Release);
                                        }
                                        SessionActivity::KeepAliveFailed(code) => {
                                            let _ = shared.last_error.lock().map(|mut e| *e = Some(format!("keepalive failed: {code}")));
                                        }
                                    }
                                }
                                SessionPollResult::ConnectionLost(code) => {
                                    let _ = shared.last_error.lock().map(|mut e| *e = Some(format!("connection lost: {code}")));
                                    shared.healthy.store(false, Ordering::Release);
                                    let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                                    return seen_active;
                                }
                                _ => {}
                            }
                        }
                        Some(Err(status_code)) => {
                            let _ = shared.last_error.lock().map(|mut e| *e = Some(format!("event loop error: {status_code}")));
                            shared.healthy.store(false, Ordering::Release);
                            let _ = state_tx.send(SouthwardConnectionState::Failed(format!("event loop error: {status_code}")));
                            return seen_active;
                        }
                        None => {
                            shared.healthy.store(false, Ordering::Release);
                            return seen_active;
                        }
                    }
                }
            }
        }
    }
}
