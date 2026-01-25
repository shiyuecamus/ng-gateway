use std::{
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};

use ng_gateway_error::{web::WebError, WebResult};
use tokio::net::lookup_host;

/// Returns true if an IP is disallowed as a net-debug target.
///
/// This is a minimal SSRF safety line even when audit is not enabled.
#[inline]
pub fn is_disallowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_multicast() || v4.is_broadcast() || v4.is_unspecified() {
                return true;
            }
            if v4.is_link_local() {
                return true;
            }
            // Block cloud metadata (common SSRF target).
            if v4 == Ipv4Addr::new(169, 254, 169, 254) {
                return true;
            }
            false
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_multicast() || v6.is_unspecified() {
                return true;
            }
            if v6.is_unicast_link_local() {
                return true;
            }
            false
        }
    }
}

/// Resolve a host (domain or IP literal) to a list of IPs and validate each IP.
///
/// - If `host` is an IP literal, it is validated directly.
/// - If `host` is a domain name, DNS results are validated (protects against DNS rebinding).
pub async fn resolve_and_validate_host(host: &str, port: Option<u16>) -> WebResult<Vec<IpAddr>> {
    let host = host.trim();
    if host.is_empty() {
        return Err(WebError::BadRequest("Host is required".to_string()));
    }

    // Reject obviously unsafe/invalid "host" values.
    if host.contains("://") {
        return Err(WebError::BadRequest(
            "Host should not include scheme (use raw host/IP)".to_string(),
        ));
    }

    // If it's already an IP literal, validate directly.
    if let Ok(ip) = IpAddr::from_str(host) {
        if is_disallowed_ip(ip) {
            return Err(WebError::BadRequest(format!(
                "Target IP is not allowed: {ip}"
            )));
        }
        return Ok(vec![ip]);
    }

    let port = port.unwrap_or(80);
    let mut ips: Vec<IpAddr> = Vec::new();

    // `lookup_host` returns SocketAddr entries.
    let addrs = lookup_host((host, port))
        .await
        .map_err(|e| WebError::BadRequest(format!("DNS lookup failed: {e}")))?;
    for addr in addrs {
        let ip = addr.ip();
        if is_disallowed_ip(ip) {
            return Err(WebError::BadRequest(format!(
                "Target IP is not allowed: {ip}"
            )));
        }
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }

    Ok(ips)
}
