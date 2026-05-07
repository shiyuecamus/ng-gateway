//! Self-managed PKI lifecycle for the OPC UA Server plugin.
//!
//! # Why we don't rely on `async-opcua-server`'s default keypair generation
//!
//! `async-opcua-server 0.18` will auto-create a self-signed certificate on
//! first start when `create_sample_keypair(true)` is set, but its `From`
//! conversion `(ApplicationDescription, Option<Vec<String>>) -> X509Data` is
//! always invoked with `addresses: None` (see `async-opcua-server::server::
//! Server::new_from_builder`). The resulting certificate's `subjectAltName`
//! list is therefore limited to `application_uri` + `localhost` + the local
//! hostname + the host's local NIC IPs. This breaks two important production
//! deployment shapes:
//!
//! - **Docker bridge** (`-p 4840:4840`): the container only knows its own
//!   internal IP (e.g. `172.17.0.2`); the host IP that clients actually dial
//!   (e.g. `192.168.1.10`) is never in the cert SAN. Strict OPC UA clients
//!   (KEPServerEX, UaExpert) then reject the connection with
//!   `Bad_CertificateHostNameInvalid`.
//! - **Operator-supplied alternative names** (LAN alias DNS, K8s Service VIP,
//!   reverse-proxy hostname): there is no API surface to inject these.
//!
//! By managing the certificate ourselves we get a deterministic SAN list
//! `[uri = application_uri] + [advertised hosts] + [extra alt hostnames] +
//! [localhost, 127.0.0.1, ::1]` and can react to configuration drift.
//!
//! # Self-healing model
//!
//! Each successful generation persists a `cert.intent` file containing the
//! sha256 hash of the inputs that shaped the certificate's identity:
//! `application_uri`, sorted advertised hosts, sorted extra alt hostnames.
//! On startup we compare the on-disk hash with the hash of the current
//! configuration and choose between four reconciliation decisions:
//!
//! - `GenerateFirst`        — no cert on disk yet; generate one.
//! - `RegenerateForDrift`   — cert exists but inputs changed; archive + regen.
//! - `RegenerateForExpiry`  — cert exists but `not_after - now <= 30 days`;
//!   archive + regen.
//! - `KeepExisting`         — happy path; the cert already matches and is healthy.
//!
//! Old certificates are **never deleted**: they are moved into
//! `archive/<utc-iso8601>/` along with their private key and a small
//! `reason.txt` so post-incident forensics can trace why a regen happened.
//!
//! # Daily expiry monitor
//!
//! `run_expiry_monitor` is a long-running task spawned by the connector. It
//! polls the on-disk cert every 24 hours and emits structured `tracing` logs
//! at `WARN` (≤ `warn_days`) or `ERROR` (≤ 3 days, or already expired).

use crate::protocol::EndpointAddr;
use chrono::{DateTime, Utc};
use ng_gateway_sdk::{log::fields as log_fields, NorthwardError, NorthwardResult};
use opcua::crypto::{CertificateStore, X509Data, X509};
use opcua::types::{ApplicationDescription, ApplicationType, LocalizedText, UAString};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::{Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Relative path of the application instance certificate (DER) inside `pki_dir`.
const CERT_REL_PATH: &str = "own/cert.der";
/// Relative path of the application instance private key (PEM) inside `pki_dir`.
const PKEY_REL_PATH: &str = "private/private.pem";
/// Relative path of the cert intent hash sidecar file inside `pki_dir`.
const INTENT_REL_PATH: &str = "own/cert.intent";
/// Relative directory where superseded certificates are archived.
const ARCHIVE_REL_DIR: &str = "archive";
/// Critical-level expiry threshold (in days). Certificates within this window
/// are surfaced as `tracing::error!` and trigger auto-regeneration.
const CERT_CRITICAL_EXPIRY_DAYS: i64 = 3;
/// Polling interval of `run_expiry_monitor`.
const EXPIRY_MONITOR_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Default validity period of newly minted certificates (matches async-opcua's default).
const DEFAULT_CERT_DURATION_DAYS: u32 = 365;

#[cfg(test)]
const TEST_PRODUCT_URI: &str = "urn:ng:opcua-server:test";

/// Stable inputs that fully determine the certificate's identity surface.
///
/// Two `CertIntent` instances hash to the same value iff they would produce a
/// certificate that is interchangeable for OPC UA validation purposes.
#[derive(Debug, Clone)]
pub struct CertIntent {
    /// Server `ApplicationUri`; becomes the first URI SAN entry.
    pub application_uri: String,
    /// Hosts extracted from `advertised_endpoints` (sorted, deduplicated).
    /// Each becomes a DNS or IP SAN depending on parsing.
    ///
    /// This list is the single source of truth for the certificate's
    /// `subjectAltName` host coverage — operators add every hostname / IP a
    /// client might dial directly to `advertised_endpoints` and the PKI layer
    /// picks them up from here.
    pub advertised_hosts: Vec<String>,
}

impl CertIntent {
    /// Build a `CertIntent` from typed inputs, performing sort + dedup so the
    /// resulting hash is order-insensitive.
    pub fn new(application_uri: impl Into<String>, advertised: &[EndpointAddr]) -> Self {
        let mut advertised_hosts: Vec<String> = advertised.iter().map(|e| e.host.clone()).collect();
        advertised_hosts.sort();
        advertised_hosts.dedup();

        Self {
            application_uri: application_uri.into(),
            advertised_hosts,
        }
    }

    /// SHA-256 hex digest of `(application_uri, sorted hosts)`.
    ///
    /// The format pinned here is part of the on-disk contract — changing it
    /// would invalidate every existing `cert.intent` file in the field. Use a
    /// new sidecar filename if a future revision needs a different format.
    pub fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"opcua-server-cert-intent:v1\n");
        hasher.update(b"application_uri=");
        hasher.update(self.application_uri.as_bytes());
        hasher.update(b"\n");
        hasher.update(b"advertised_hosts=");
        for h in &self.advertised_hosts {
            hasher.update(h.as_bytes());
            hasher.update(b"\x1f"); // ASCII unit separator, never legal inside a hostname
        }
        hex::encode(hasher.finalize())
    }

    /// All hostnames + IPs that should land in the certificate's SAN list.
    ///
    /// `localhost`, `127.0.0.1`, `::1` are always included so loopback / dev
    /// connections work out of the box.
    pub fn san_addresses(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for h in &self.advertised_hosts {
            push_unique(&mut out, h.clone());
        }
        push_unique(&mut out, "localhost".to_string());
        push_unique(&mut out, "127.0.0.1".to_string());
        push_unique(&mut out, "::1".to_string());
        out
    }
}

fn push_unique(out: &mut Vec<String>, item: String) {
    if !out.iter().any(|x| x.eq_ignore_ascii_case(&item)) {
        out.push(item);
    }
}

/// Plugin-controlled summary of the on-disk certificate, surfaced through the
/// inspector capability so operators can sanity-check from the UI / API.
#[derive(Debug, Clone)]
pub struct CertSummary {
    /// SHA-1 hex thumbprint (matches what KepServer / UaExpert display).
    pub thumbprint_hex: String,
    /// X.500 Common Name (typically the gateway application name).
    pub common_name: String,
    /// First URI SAN; should equal `application_uri`.
    pub san_uri: String,
    /// DNS / hostname SAN entries (de-duped from `CertIntent`).
    pub san_hostnames: Vec<String>,
    /// IP-literal SAN entries (de-duped from `CertIntent`).
    pub san_ips: Vec<String>,
    /// Validity period start.
    pub not_before: DateTime<Utc>,
    /// Validity period end.
    pub not_after: DateTime<Utc>,
    /// Days until expiry (negative if already expired).
    pub days_to_expiry: i64,
    /// One of `"healthy"`, `"expiring"`, `"expired"`.
    pub health: &'static str,
}

impl CertSummary {
    /// Build a summary from a parsed cert + the intent that shaped its SAN.
    pub fn build(cert: &X509, intent: &CertIntent, warn_days: u32) -> Result<Self, NorthwardError> {
        let common_name = cert
            .common_name()
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("certificate common name not readable: {e}"),
            })?;
        let not_before = cert
            .not_before()
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("certificate not_before not readable: {e}"),
            })?;
        let not_after = cert
            .not_after()
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("certificate not_after not readable: {e}"),
            })?;
        let now = Utc::now();
        let days_to_expiry = (not_after - now).num_days();
        let health = if days_to_expiry < 0 {
            "expired"
        } else if days_to_expiry <= warn_days as i64 {
            "expiring"
        } else {
            "healthy"
        };

        let (san_hostnames, san_ips) = split_hosts_and_ips(&intent.san_addresses());

        Ok(Self {
            thumbprint_hex: cert.thumbprint().as_hex_string(),
            common_name,
            san_uri: intent.application_uri.clone(),
            san_hostnames,
            san_ips,
            not_before,
            not_after,
            days_to_expiry,
            health,
        })
    }
}

fn split_hosts_and_ips(addresses: &[String]) -> (Vec<String>, Vec<String>) {
    let mut hostnames = Vec::new();
    let mut ips = Vec::new();
    for raw in addresses {
        if Ipv4Addr::from_str(raw).is_ok() || Ipv6Addr::from_str(raw).is_ok() {
            ips.push(raw.clone());
        } else {
            hostnames.push(raw.clone());
        }
    }
    (hostnames, ips)
}

/// What the reconciliation decided to do with the on-disk certificate.
#[derive(Debug, Clone)]
pub enum ReconcileDecision {
    /// No certificate file present; generate a fresh one.
    GenerateFirst,
    /// Cert exists but the recorded intent hash no longer matches.
    RegenerateForDrift {
        /// Human-readable explanation, included in the archive's `reason.txt`.
        reason: String,
    },
    /// Cert exists, intent matches, but it expires within `warn_days`.
    RegenerateForExpiry { days_to_expiry: i64 },
    /// Cert exists, intent matches, and validity is comfortably in range.
    KeepExisting { days_to_expiry: i64 },
}

/// Compare on-disk PKI state with the desired `intent`, returning the action
/// the caller should take **before** `ServerBuilder.build()` is invoked.
///
/// This function performs file-system reads only; it never writes.
pub fn reconcile(
    pki_dir: &Path,
    intent: &CertIntent,
    warn_days: u32,
) -> NorthwardResult<ReconcileDecision> {
    let cert_path = pki_dir.join(CERT_REL_PATH);
    let pkey_path = pki_dir.join(PKEY_REL_PATH);
    let intent_path = pki_dir.join(INTENT_REL_PATH);

    if !cert_path.exists() || !pkey_path.exists() {
        return Ok(ReconcileDecision::GenerateFirst);
    }

    let recorded_intent = read_intent_file(&intent_path)?;
    let want_hash = intent.hash();
    let recorded_hash_matches = recorded_intent.as_deref().map(str::trim) == Some(&want_hash);

    if !recorded_hash_matches {
        let reason = match recorded_intent.as_deref() {
            Some(prev) => format!(
                "configuration drift detected (recorded intent hash {} != current {})",
                prev.trim(),
                want_hash
            ),
            None => format!(
                "missing or unreadable cert.intent sidecar; assuming drift to current intent {}",
                want_hash
            ),
        };
        return Ok(ReconcileDecision::RegenerateForDrift { reason });
    }

    let der = fs::read(&cert_path).map_err(|e| NorthwardError::ConfigurationError {
        message: format!(
            "failed to read existing certificate {}: {e}",
            cert_path.display()
        ),
    })?;
    let cert = X509::from_der(&der).map_err(|e| NorthwardError::ConfigurationError {
        message: format!(
            "existing certificate at {} is invalid: {e}",
            cert_path.display()
        ),
    })?;
    let not_after = cert
        .not_after()
        .map_err(|e| NorthwardError::ConfigurationError {
            message: format!(
                "existing certificate at {} has unreadable not_after: {e}",
                cert_path.display()
            ),
        })?;
    let days_to_expiry = (not_after - Utc::now()).num_days();
    if days_to_expiry <= warn_days as i64 {
        return Ok(ReconcileDecision::RegenerateForExpiry { days_to_expiry });
    }

    Ok(ReconcileDecision::KeepExisting { days_to_expiry })
}

/// Read the `cert.intent` sidecar.
///
/// `Ok(None)` when the file is absent — distinct from `Err(_)` which signals
/// an actual IO problem the caller should bubble up.
fn read_intent_file(intent_path: &Path) -> NorthwardResult<Option<String>> {
    if !intent_path.exists() {
        return Ok(None);
    }
    fs::read_to_string(intent_path)
        .map(Some)
        .map_err(|e| NorthwardError::ConfigurationError {
            message: format!(
                "failed to read certificate intent sidecar {}: {e}",
                intent_path.display()
            ),
        })
}

/// Write the current intent hash to `pki_dir/own/cert.intent`.
fn write_intent_file(pki_dir: &Path, hash: &str) -> NorthwardResult<()> {
    let path = pki_dir.join(INTENT_REL_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| NorthwardError::ConfigurationError {
            message: format!("failed to create directory {}: {e}", parent.display()),
        })?;
    }
    fs::write(&path, hash).map_err(|e| NorthwardError::ConfigurationError {
        message: format!("failed to write {} : {e}", path.display()),
    })
}

/// Move the existing cert + private key into `archive/<utc-iso8601>/` and
/// drop a `reason.txt` describing why the regen happened.
///
/// Idempotent: if the cert/pkey are not present, returns `Ok(())` silently so
/// `RegenerateForDrift` after a partial first run still succeeds.
pub fn archive_existing(pki_dir: &Path, reason: &str) -> NorthwardResult<()> {
    let cert_path = pki_dir.join(CERT_REL_PATH);
    let pkey_path = pki_dir.join(PKEY_REL_PATH);
    let intent_path = pki_dir.join(INTENT_REL_PATH);

    if !cert_path.exists() && !pkey_path.exists() {
        return Ok(());
    }

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let archive_subdir = pki_dir.join(ARCHIVE_REL_DIR).join(&timestamp);
    fs::create_dir_all(&archive_subdir).map_err(|e| NorthwardError::ConfigurationError {
        message: format!(
            "failed to create archive directory {}: {e}",
            archive_subdir.display()
        ),
    })?;

    if cert_path.exists() {
        let dest = archive_subdir.join("cert.der");
        fs::rename(&cert_path, &dest).map_err(|e| NorthwardError::ConfigurationError {
            message: format!(
                "failed to archive certificate {} -> {}: {e}",
                cert_path.display(),
                dest.display()
            ),
        })?;
    }
    if pkey_path.exists() {
        let dest = archive_subdir.join("private.pem");
        fs::rename(&pkey_path, &dest).map_err(|e| NorthwardError::ConfigurationError {
            message: format!(
                "failed to archive private key {} -> {}: {e}",
                pkey_path.display(),
                dest.display()
            ),
        })?;
    }
    if intent_path.exists() {
        let dest = archive_subdir.join("cert.intent");
        let _ = fs::rename(&intent_path, &dest);
    }
    let reason_path = archive_subdir.join("reason.txt");
    if let Err(e) = fs::write(&reason_path, reason.as_bytes()) {
        warn!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            archive = %archive_subdir.display(),
            error = %e,
            "Failed to write archive reason.txt; continuing"
        );
    }

    info!(
        target: log_fields::TARGET_PLUGIN,
        source = log_fields::SOURCE_PLUGIN,
        plugin_type = "opcua-server",
        archive = %archive_subdir.display(),
        "OPC UA Server: archived superseded PKI artifacts"
    );
    Ok(())
}

/// Generate a new application instance certificate with a fully-customised
/// SAN list, persist it to `pki_dir/own/cert.der` + `pki_dir/private/private.pem`,
/// and record the intent hash.
///
/// Caller is expected to have already invoked `archive_existing` if a
/// pre-existing cert needed to be preserved.
pub fn generate_and_persist(
    pki_dir: &Path,
    application_name: &str,
    product_uri: &str,
    intent: &CertIntent,
) -> NorthwardResult<X509> {
    ensure_pki_dirs(pki_dir)?;

    // We synthesise an `ApplicationDescription` and feed it into
    // `async-opcua-crypto`'s public `From<(ApplicationDescription,
    // Option<Vec<String>>)> for X509Data` impl. That impl is what guarantees:
    //   - the first SAN entry is `application_uri` (URI form),
    //   - `addresses` we supply are added (parsed as IP or DNS automatically),
    //   - `localhost`, the local computer name and host NIC IPs are added too.
    //
    // The auto-added entries are strictly additive (only ever expand the
    // accepted hostname/IP set), so they are harmless even in Docker bridge
    // mode. The crucial fix versus the upstream default is that we NOW pass
    // the operator-controlled list via `addresses`, instead of `None`.
    let app_desc = ApplicationDescription {
        application_uri: UAString::from(intent.application_uri.as_str()),
        product_uri: UAString::from(product_uri),
        application_name: LocalizedText::new("", application_name),
        application_type: ApplicationType::Server,
        gateway_server_uri: UAString::null(),
        discovery_profile_uri: UAString::null(),
        discovery_urls: None,
    };
    let mut x509_data: X509Data = (app_desc, Some(intent.san_addresses())).into();
    // Force CN / O / OU back to the operator-visible application name so the
    // certificate self-renders consistently in client-side trust dialogs.
    x509_data.common_name = application_name.to_string();
    x509_data.organization = application_name.to_string();
    x509_data.organizational_unit = application_name.to_string();
    x509_data.certificate_duration_days = DEFAULT_CERT_DURATION_DAYS;

    let cert_path = pki_dir.join(CERT_REL_PATH);
    let pkey_path = pki_dir.join(PKEY_REL_PATH);
    let (cert, _pkey) = CertificateStore::create_certificate_and_key(
        &x509_data, true, // overwrite — caller already archived the prior pair if any
        &cert_path, &pkey_path,
    )
    .map_err(|e| NorthwardError::ConfigurationError {
        message: format!("failed to generate self-signed certificate: {e}"),
    })?;

    write_intent_file(pki_dir, &intent.hash())?;
    Ok(cert)
}

/// One-shot PKI bootstrap that owns the full reconcile / archive / generate /
/// load / summary pipeline.
///
/// This is the **single entry point** the connector uses to materialise an
/// on-disk application instance certificate. It guarantees that, on success:
///
/// 1. `pki_dir/own/cert.der` and `pki_dir/private/private.pem` exist and match
///    the supplied `intent` (i.e. drift / expiry have already been resolved).
/// 2. A fresh `CertSummary` describing that on-disk pair has been computed
///    and an INFO-level `OPC UA Server PKI summary` log line has been emitted
///    so first-line operator triage works without poking the file system.
///
/// # Why it lives at connector scope (not session scope)
///
/// The OPC UA Server plugin used to perform reconcile + RSA key generation
/// inside `Connector::connect()` — but RSA-2048 keypair generation is a
/// blocking, CPU-bound operation that can take **multiple seconds** in
/// debug builds (under `lldb`/`gdb`) or on weak hardware. That work
/// previously fell **inside** the host's
/// `start_app_with_policy(SyncWaitConnected { timeout_ms })` window, so
/// adding a brand-new app from the API used to surface as `Plugin
/// connection timeout after 5000 ms` — even though the server actually
/// finished bootstrap a few tens of milliseconds later.
///
/// Promoting this work to `Connector::new` (i.e. `from_init`) takes the
/// PKI cost **out of the connect-timeout window**: by the time the
/// supervisor calls `connect()` on the first attempt, the cert is already
/// on disk and `reconcile()` returns `KeepExisting`, so `connect()`
/// completes in milliseconds.
///
/// # Notes
/// - This function is fully synchronous (no `tokio::spawn`, no async I/O)
///   and may block for up to a few seconds on the very first call per
///   `app_id` while a fresh RSA keypair is generated. Subsequent calls
///   for the same `app_id` go through the `KeepExisting` fast path.
/// - **Callers MUST off-load this onto a blocking pool**, e.g. via
///   [`tokio::task::spawn_blocking`], when invoking from an async context
///   (the connector does this inside `OpcuaServerConnector::from_init`).
///   Calling it directly from an async fn would block a tokio worker for
///   the duration of keygen.
pub fn prepare_for_runtime(
    pki_dir: &Path,
    application_name: &str,
    product_uri: &str,
    intent: &CertIntent,
    warn_days: u32,
    app_id: i32,
) -> NorthwardResult<CertSummary> {
    ensure_pki_dirs(pki_dir)?;

    let decision = reconcile(pki_dir, intent, warn_days)?;
    match &decision {
        ReconcileDecision::GenerateFirst => {
            info!(
                target: log_fields::TARGET_PLUGIN,
                source = log_fields::SOURCE_PLUGIN,
                plugin_type = "opcua-server",
                app_id = app_id,
                "opcua-server PKI: no certificate on disk; generating fresh self-signed pair"
            );
        }
        ReconcileDecision::RegenerateForDrift { reason } => {
            warn!(
                target: log_fields::TARGET_PLUGIN,
                source = log_fields::SOURCE_PLUGIN,
                plugin_type = "opcua-server",
                app_id = app_id,
                reason = %reason,
                "opcua-server PKI: configuration drift; archiving old cert and regenerating"
            );
            archive_existing(pki_dir, reason)?;
        }
        ReconcileDecision::RegenerateForExpiry { days_to_expiry } => {
            warn!(
                target: log_fields::TARGET_PLUGIN,
                source = log_fields::SOURCE_PLUGIN,
                plugin_type = "opcua-server",
                app_id = app_id,
                days_to_expiry,
                "opcua-server PKI: certificate close to expiry; archiving and regenerating"
            );
            archive_existing(
                pki_dir,
                &format!("renewed before expiry: {days_to_expiry} days remaining"),
            )?;
        }
        ReconcileDecision::KeepExisting { days_to_expiry } => {
            info!(
                target: log_fields::TARGET_PLUGIN,
                source = log_fields::SOURCE_PLUGIN,
                plugin_type = "opcua-server",
                app_id = app_id,
                days_to_expiry,
                "opcua-server PKI: certificate intent matches and is healthy; reusing"
            );
        }
    }

    let need_generate = !matches!(decision, ReconcileDecision::KeepExisting { .. });
    if need_generate {
        // RSA-2048 keypair generation + X509 self-sign happens here. This
        // is the one-time cost we pulled out of the connect-timeout path.
        generate_and_persist(pki_dir, application_name, product_uri, intent)?;
    }

    let cert = load_cert(pki_dir)?.ok_or_else(|| NorthwardError::ConfigurationError {
        message: format!(
            "expected certificate at {} after reconcile but none was loaded",
            pki_dir.display()
        ),
    })?;
    let summary = CertSummary::build(&cert, intent, warn_days)?;
    log_pki_summary(&summary, app_id);
    Ok(summary)
}

/// Make sure all directories under `pki_dir` that the lifecycle touches exist.
pub fn ensure_pki_dirs(pki_dir: &Path) -> NorthwardResult<()> {
    for sub in &[
        "own",
        "private",
        "trusted",
        "rejected",
        "issuers",
        ARCHIVE_REL_DIR,
    ] {
        let dir = pki_dir.join(sub);
        fs::create_dir_all(&dir).map_err(|e| NorthwardError::ConfigurationError {
            message: format!("failed to create PKI subdir {}: {e}", dir.display()),
        })?;
    }
    Ok(())
}

/// Read the on-disk certificate (if any) and parse it.
pub fn load_cert(pki_dir: &Path) -> NorthwardResult<Option<X509>> {
    let cert_path = pki_dir.join(CERT_REL_PATH);
    if !cert_path.exists() {
        return Ok(None);
    }
    let der = fs::read(&cert_path).map_err(|e| NorthwardError::ConfigurationError {
        message: format!("failed to read certificate {}: {e}", cert_path.display()),
    })?;
    let cert = X509::from_der(&der).map_err(|e| NorthwardError::ConfigurationError {
        message: format!("certificate at {} is invalid: {e}", cert_path.display()),
    })?;
    Ok(Some(cert))
}

/// Emit a single `INFO`-level log line summarising the live certificate, so
/// any first-line operator triage of "why doesn't KepServer connect?" has the
/// thumbprint, validity, and full SAN footprint at hand.
pub fn log_pki_summary(summary: &CertSummary, app_id: i32) {
    info!(
        target: log_fields::TARGET_PLUGIN,
        source = log_fields::SOURCE_PLUGIN,
        plugin_type = "opcua-server",
        app_id = app_id,
        thumbprint = %summary.thumbprint_hex,
        common_name = %summary.common_name,
        san_uri = %summary.san_uri,
        san_hostnames = ?summary.san_hostnames,
        san_ips = ?summary.san_ips,
        not_before = %summary.not_before,
        not_after = %summary.not_after,
        days_to_expiry = summary.days_to_expiry,
        health = summary.health,
        "OPC UA Server PKI summary"
    );
}

/// Long-running daily monitor that re-reads the on-disk certificate and emits
/// `WARN` (≤ `warn_days`) or `ERROR` (≤ 3 days, or expired) `tracing` events.
///
/// Lives at connector scope (not session scope) so it survives reconnect
/// attempts; cancelled when the connector shuts down via `shutdown`.
pub async fn run_expiry_monitor(
    pki_dir: PathBuf,
    application_uri: String,
    warn_days: u32,
    app_id: i32,
    shutdown: CancellationToken,
) {
    let mut tick = interval(EXPIRY_MONITOR_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                info!(
                    target: log_fields::TARGET_PLUGIN,
                    source = log_fields::SOURCE_PLUGIN,
                    plugin_type = "opcua-server",
                    app_id = app_id,
                    "OPC UA Server certificate expiry monitor stopped"
                );
                break;
            }
            _ = tick.tick() => {
                inspect_and_emit(&pki_dir, &application_uri, warn_days, app_id);
            }
        }
    }
}

fn inspect_and_emit(pki_dir: &Path, application_uri: &str, warn_days: u32, app_id: i32) {
    let cert = match load_cert(pki_dir) {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(
                target: log_fields::TARGET_PLUGIN,
                source = log_fields::SOURCE_PLUGIN,
                plugin_type = "opcua-server",
                app_id = app_id,
                "OPC UA Server expiry monitor: certificate file not present"
            );
            return;
        }
        Err(e) => {
            error!(
                target: log_fields::TARGET_PLUGIN,
                source = log_fields::SOURCE_PLUGIN,
                plugin_type = "opcua-server",
                app_id = app_id,
                error = %e,
                "OPC UA Server expiry monitor: failed to load certificate"
            );
            return;
        }
    };
    let not_after = match cert.not_after() {
        Ok(t) => t,
        Err(e) => {
            error!(
                target: log_fields::TARGET_PLUGIN,
                source = log_fields::SOURCE_PLUGIN,
                plugin_type = "opcua-server",
                app_id = app_id,
                error = ?e,
                "OPC UA Server expiry monitor: not_after unreadable"
            );
            return;
        }
    };
    let days_to_expiry = (not_after - Utc::now()).num_days();
    let thumbprint = cert.thumbprint().as_hex_string();

    if days_to_expiry < 0 {
        error!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            application_uri = %application_uri,
            thumbprint = %thumbprint,
            not_after = %not_after,
            days_to_expiry,
            "OPC UA Server certificate has EXPIRED; \
             auto-regeneration will fire on next plugin restart"
        );
    } else if days_to_expiry <= CERT_CRITICAL_EXPIRY_DAYS {
        error!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            application_uri = %application_uri,
            thumbprint = %thumbprint,
            not_after = %not_after,
            days_to_expiry,
            "OPC UA Server certificate critically close to expiry; \
             restart the plugin to trigger auto-regeneration"
        );
    } else if days_to_expiry <= warn_days as i64 {
        warn!(
            target: log_fields::TARGET_PLUGIN,
            source = log_fields::SOURCE_PLUGIN,
            plugin_type = "opcua-server",
            app_id = app_id,
            application_uri = %application_uri,
            thumbprint = %thumbprint,
            not_after = %not_after,
            days_to_expiry,
            "OPC UA Server certificate approaching expiry"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::parse_advertised_endpoint;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("ng-opcua-pki-{label}-{nanos}-{seq}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn endpoint(raw: &str) -> EndpointAddr {
        parse_advertised_endpoint(raw).unwrap()
    }

    #[test]
    fn cert_intent_hash_is_order_insensitive() {
        let advertised = vec![
            endpoint("opc.tcp://gateway.local:4840/"),
            endpoint("opc.tcp://192.168.1.10:4840/"),
        ];

        let a = CertIntent::new("urn:ng:opcua-server", &advertised);
        let b = CertIntent::new(
            "urn:ng:opcua-server",
            &[
                endpoint("opc.tcp://192.168.1.10:4840/"),
                endpoint("opc.tcp://gateway.local:4840/"),
            ],
        );
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn cert_intent_hash_changes_on_uri_change() {
        let endpoints = vec![endpoint("opc.tcp://gateway.local:4840/")];
        let a = CertIntent::new("urn:ng:opcua-server", &endpoints);
        let b = CertIntent::new("urn:ng:opcua-server:v2", &endpoints);
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn cert_intent_hash_changes_on_advertised_host_change() {
        let a = CertIntent::new(
            "urn:ng:opcua-server",
            &[endpoint("opc.tcp://gateway.local:4840/")],
        );
        let b = CertIntent::new(
            "urn:ng:opcua-server",
            &[endpoint("opc.tcp://gateway.lan:4840/")],
        );
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn cert_intent_san_addresses_includes_loopback() {
        let endpoints = vec![endpoint("opc.tcp://192.168.1.10:4840/")];
        let intent = CertIntent::new("urn:ng:opcua-server", &endpoints);
        let addrs = intent.san_addresses();
        assert!(addrs.contains(&"192.168.1.10".to_string()));
        assert!(addrs.contains(&"localhost".to_string()));
        assert!(addrs.contains(&"127.0.0.1".to_string()));
        assert!(addrs.contains(&"::1".to_string()));
    }

    #[test]
    fn cert_intent_san_addresses_dedupes_case_insensitive() {
        let endpoints = vec![
            endpoint("opc.tcp://Gateway.Local:4840/"),
            endpoint("opc.tcp://gateway.LOCAL:4841/"),
        ];
        let intent = CertIntent::new("urn:ng", &endpoints);
        let addrs = intent.san_addresses();
        // Both inputs are the same host (case-insensitive), so SAN list
        // must include exactly one of them, not both.
        let count = addrs
            .iter()
            .filter(|a| a.eq_ignore_ascii_case("Gateway.Local"))
            .count();
        assert_eq!(count, 1, "duplicate hostnames should be collapsed");
    }

    #[test]
    fn split_hosts_and_ips_categorises_correctly() {
        let (h, i) = split_hosts_and_ips(&[
            "gateway.local".to_string(),
            "192.168.1.10".to_string(),
            "::1".to_string(),
            "fe80::1".to_string(),
            "localhost".to_string(),
        ]);
        assert!(h.contains(&"gateway.local".to_string()));
        assert!(h.contains(&"localhost".to_string()));
        assert!(i.contains(&"192.168.1.10".to_string()));
        assert!(i.contains(&"::1".to_string()));
        assert!(i.contains(&"fe80::1".to_string()));
    }

    #[test]
    fn reconcile_returns_generate_first_when_empty() {
        let dir = temp_dir("first");
        let intent = CertIntent::new(
            "urn:ng:opcua-server",
            &[endpoint("opc.tcp://gateway.local:4840/")],
        );
        let decision = reconcile(&dir, &intent, 30).unwrap();
        assert!(matches!(decision, ReconcileDecision::GenerateFirst));
    }

    #[test]
    fn reconcile_keep_existing_after_generate() {
        let dir = temp_dir("keep");
        let intent = CertIntent::new(
            "urn:ng:opcua-server",
            &[endpoint("opc.tcp://gateway.local:4840/")],
        );
        let _ = generate_and_persist(&dir, "ng-gateway-test", TEST_PRODUCT_URI, &intent).unwrap();
        let decision = reconcile(&dir, &intent, 30).unwrap();
        assert!(matches!(decision, ReconcileDecision::KeepExisting { .. }));
    }

    #[test]
    fn reconcile_drift_when_intent_changes() {
        let dir = temp_dir("drift");
        let intent_a = CertIntent::new(
            "urn:ng:opcua-server",
            &[endpoint("opc.tcp://gateway.local:4840/")],
        );
        let _ = generate_and_persist(&dir, "ng-gateway-test", TEST_PRODUCT_URI, &intent_a).unwrap();
        let intent_b = CertIntent::new(
            "urn:ng:opcua-server:v2", // application_uri changed -> drift
            &[endpoint("opc.tcp://gateway.local:4840/")],
        );
        let decision = reconcile(&dir, &intent_b, 30).unwrap();
        assert!(matches!(
            decision,
            ReconcileDecision::RegenerateForDrift { .. }
        ));
    }

    #[test]
    fn archive_existing_is_idempotent_when_no_files() {
        let dir = temp_dir("archive-empty");
        archive_existing(&dir, "no-op").unwrap();
    }

    #[test]
    fn archive_existing_moves_cert_and_pkey() {
        let dir = temp_dir("archive-move");
        let intent = CertIntent::new(
            "urn:ng:opcua-server",
            &[endpoint("opc.tcp://gateway.local:4840/")],
        );
        generate_and_persist(&dir, "ng-gateway-test", TEST_PRODUCT_URI, &intent).unwrap();
        archive_existing(&dir, "test reason").unwrap();
        assert!(!dir.join(CERT_REL_PATH).exists());
        assert!(!dir.join(PKEY_REL_PATH).exists());
        // archive subdir should exist with cert.der inside
        let archive_root = dir.join(ARCHIVE_REL_DIR);
        let entries: Vec<_> = fs::read_dir(&archive_root).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let archive_sub = entries.into_iter().next().unwrap().unwrap().path();
        assert!(archive_sub.join("cert.der").exists());
        assert!(archive_sub.join("private.pem").exists());
        assert!(archive_sub.join("reason.txt").exists());
    }

    #[test]
    fn cert_summary_classifies_health() {
        let dir = temp_dir("summary");
        let intent = CertIntent::new(
            "urn:ng:opcua-server",
            &[
                endpoint("opc.tcp://gateway.local:4840/"),
                endpoint("opc.tcp://192.168.1.10:4840/"),
            ],
        );
        let cert =
            generate_and_persist(&dir, "ng-gateway-test", TEST_PRODUCT_URI, &intent).unwrap();
        let summary = CertSummary::build(&cert, &intent, 30).unwrap();
        assert_eq!(summary.health, "healthy");
        assert_eq!(summary.san_uri, "urn:ng:opcua-server");
        assert!(summary.san_hostnames.contains(&"gateway.local".to_string()));
        assert!(summary.san_ips.contains(&"192.168.1.10".to_string()));
        assert!(summary.san_ips.contains(&"127.0.0.1".to_string()));
        assert!(summary.san_ips.contains(&"::1".to_string()));
        assert!(!summary.thumbprint_hex.is_empty());
    }
}
