use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, net::IpAddr};
use validator::Validate;

/// Ping probing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PingMode {
    /// ICMP echo (may require raw socket privileges on Linux).
    #[default]
    Icmp,
    /// TCP connect "ping" (works without raw socket privileges).
    Tcp,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PingRequest {
    /// Hostname or IP.
    #[validate(length(min = 1, message = "host is required"))]
    pub host: String,
    /// How many probes to send.
    #[validate(range(min = 1, max = 20, message = "count must be in [1, 20]"))]
    pub count: Option<u16>,
    /// Timeout per probe.
    #[validate(range(min = 100, max = 120_000, message = "timeoutMs out of range"))]
    pub timeout_ms: Option<u64>,
    /// Interval between probes.
    #[validate(range(min = 0, max = 10_000, message = "intervalMs out of range"))]
    pub interval_ms: Option<u64>,
    /// Probe mode.
    pub mode: Option<PingMode>,
    /// TCP port used when mode = tcp.
    #[validate(range(min = 1, max = 65_535, message = "tcpPort out of range"))]
    pub tcp_port: Option<u16>,
    /// Payload size for ICMP (bytes).
    #[validate(range(min = 8, max = 1024, message = "payloadBytes out of range"))]
    pub payload_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingSample {
    pub seq: u16,
    pub ok: bool,
    pub rtt_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResponse {
    pub host: String,
    pub resolved_ips: Vec<IpAddr>,
    pub target_ip: Option<IpAddr>,
    pub mode: PingMode,
    pub tcp_port: Option<u16>,
    pub sent: u16,
    pub received: u16,
    pub loss_percent: f64,
    pub rtt_min_ms: Option<u64>,
    pub rtt_avg_ms: Option<u64>,
    pub rtt_max_ms: Option<u64>,
    pub samples: Vec<PingSample>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct TcpConnectRequest {
    #[validate(length(min = 1, message = "host is required"))]
    pub host: String,
    #[validate(range(min = 1, max = 65_535, message = "port out of range"))]
    pub port: u16,
    #[validate(range(min = 100, max = 120_000, message = "timeoutMs out of range"))]
    pub timeout_ms: Option<u64>,
    /// If true, attempt to read a small banner after connect.
    pub read_banner: Option<bool>,
    /// Max bytes to read for banner.
    #[validate(range(min = 1, max = 4096, message = "bannerBytes out of range"))]
    pub banner_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TcpConnectResponse {
    pub host: String,
    pub port: u16,
    pub resolved_ips: Vec<IpAddr>,
    pub target_ip: Option<IpAddr>,
    pub connected: bool,
    pub connect_ms: Option<u64>,
    pub banner: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    GET,
    POST,
    PUT,
    PATCH,
    DELETE,
    HEAD,
    OPTIONS,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequest {
    pub method: HttpMethod,
    #[validate(length(min = 1, message = "url is required"))]
    pub url: String,
    /// Request headers (key/value). Duplicate keys are allowed and will be sent in order.
    pub headers: Option<Vec<(String, String)>>,
    /// Request body (UTF-8).
    pub body: Option<String>,
    #[validate(range(min = 200, max = 300_000, message = "timeoutMs out of range"))]
    pub timeout_ms: Option<u64>,
    pub follow_redirects: Option<bool>,
    pub insecure_tls: Option<bool>,
    /// Max response bytes to read (hard limit).
    #[validate(range(min = 1024, max = 2_097_152, message = "maxResponseBytes out of range"))]
    pub max_response_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResponse {
    pub url: String,
    pub resolved_ips: Vec<IpAddr>,
    pub status: Option<u16>,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    pub body_truncated: bool,
    pub total_ms: u64,
    pub error: Option<String>,
}
