//! Network debugging endpoints (ping / tcp-connect / http-request).
//!
//! Notes
//! - This module intentionally does **not** execute shell commands like `ping` / `telnet` / `curl`.
//!   It uses libraries (ICMP + TCP sockets + HTTP client) to avoid command injection risks.
//! - Even without audit logs, we still enforce basic SSRF safety checks to avoid turning the
//!   gateway into an internal scanning tool.

use std::{collections::BTreeMap, time::Instant};

use crate::{middleware::RequestContext, rbac::has_any_role};
use actix_web::{http::Method, web};
use actix_web_validator::Json;
use futures::StreamExt;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        HttpMethod, HttpRequest, HttpResponse, PingMode, PingRequest, PingResponse, PingSample,
        TcpConnectRequest, TcpConnectResponse,
    },
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_utils::net::resolve_and_validate_host;
use tokio::{
    net::TcpStream,
    time::{sleep, timeout, Duration},
};
use tracing::{info, instrument};
use url::Url;

pub(super) const ROUTER_PREFIX: &str = "/net-debug";

/// Configure net-debug routes.
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/ping", web::post().to(ping))
        .route("/tcp", web::post().to(tcp_connect))
        .route("/http", web::post().to(http_request));
}

/// Initialize RBAC rules for net-debug module (admin only for now).
#[inline]
#[instrument(name = "init-net-debug-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing net-debug module RBAC rules...");

    let rules: [(Method, String, Box<dyn PermRule>); 3] = [
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/ping"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/tcp"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/http"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
    ];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    info!("Net-debug module RBAC rules initialized successfully");
    Ok(())
}

#[inline]
fn to_reqwest_method(method: HttpMethod) -> reqwest::Method {
    match method {
        HttpMethod::GET => reqwest::Method::GET,
        HttpMethod::POST => reqwest::Method::POST,
        HttpMethod::PUT => reqwest::Method::PUT,
        HttpMethod::PATCH => reqwest::Method::PATCH,
        HttpMethod::DELETE => reqwest::Method::DELETE,
        HttpMethod::HEAD => reqwest::Method::HEAD,
        HttpMethod::OPTIONS => reqwest::Method::OPTIONS,
    }
}

#[instrument(name = "net-debug-ping", skip(_ctx, payload))]
async fn ping(
    _ctx: RequestContext,
    payload: Json<PingRequest>,
) -> WebResult<WebResponse<PingResponse>> {
    let payload = payload.into_inner();

    let count: u16 = payload.count.unwrap_or(4).clamp(1, 20);
    let timeout_ms: u64 = payload.timeout_ms.unwrap_or(1000).clamp(100, 30_000);
    let interval_ms: u64 = payload.interval_ms.unwrap_or(250).clamp(0, 10_000);
    let mode: PingMode = payload.mode.unwrap_or_default();
    let tcp_port: u16 = payload.tcp_port.unwrap_or(80).clamp(1, 65_535);
    let payload_bytes: usize = payload.payload_bytes.unwrap_or(32).clamp(8, 1024);

    let resolved = resolve_and_validate_host(&payload.host, Some(tcp_port)).await?;
    let target_ip = resolved.first().copied();

    let mut samples: Vec<PingSample> = Vec::with_capacity(count as usize);
    let mut received: u16 = 0;
    let mut rtts: Vec<u64> = Vec::new();
    let mut note: Option<String> = None;

    let ip = match target_ip {
        Some(ip) => ip,
        None => return Err(WebError::BadRequest("Unable to resolve host".to_string())),
    };

    for seq in 0..count {
        let start = Instant::now();

        let res = match mode {
            PingMode::Icmp => {
                // ICMP echo may fail in containers without CAP_NET_RAW.
                let payload_buf = vec![0u8; payload_bytes];
                timeout(
                    Duration::from_millis(timeout_ms),
                    surge_ping::ping(ip, payload_buf.as_slice()),
                )
                .await
                .map_err(|_| WebError::BadRequest("Ping timeout".to_string()))
                .and_then(|r| r.map_err(|e| WebError::BadRequest(format!("{e:?}"))))
                .map(|(_pkt, dur)| dur)
            }
            PingMode::Tcp => timeout(
                Duration::from_millis(timeout_ms),
                TcpStream::connect((ip, tcp_port)),
            )
            .await
            .map_err(|_| WebError::BadRequest("TCP connect timeout".to_string()))
            .and_then(|r| {
                r.map(|_| start.elapsed())
                    .map_err(|e| WebError::BadRequest(e.to_string()))
            }),
        };

        match res {
            Ok(dur) => {
                received = received.saturating_add(1);
                let rtt_ms = dur.as_millis().min(u128::from(u64::MAX)) as u64;
                rtts.push(rtt_ms);
                samples.push(PingSample {
                    seq,
                    ok: true,
                    rtt_ms: Some(rtt_ms),
                    error: None,
                });
            }
            Err(e) => {
                let msg = match mode {
                    PingMode::Icmp => {
                        // Helpful hint for common permission issue.
                        let s = e.to_string();
                        if s.contains("Permission") || s.contains("permission") {
                            note = Some(
                                "ICMP ping requires raw socket privileges (CAP_NET_RAW). Try TCP mode if running in a restricted container."
                                    .to_string(),
                            );
                        }
                        s
                    }
                    PingMode::Tcp => e.to_string(),
                };
                samples.push(PingSample {
                    seq,
                    ok: false,
                    rtt_ms: None,
                    error: Some(msg),
                });
            }
        }

        if interval_ms > 0 && seq + 1 < count {
            sleep(Duration::from_millis(interval_ms)).await;
        }
    }

    let sent = count;
    let loss_percent = if sent == 0 {
        0.0
    } else {
        ((sent - received) as f64) * 100.0 / (sent as f64)
    };

    let (min_ms, max_ms, avg_ms) = match (rtts.iter().copied().min(), rtts.iter().copied().max()) {
        (Some(min), Some(max)) => {
            let sum: u64 = rtts.iter().sum();
            let avg = sum / (rtts.len() as u64);
            (Some(min), Some(max), Some(avg))
        }
        _ => (None, None, None),
    };

    Ok(WebResponse::ok(PingResponse {
        host: payload.host,
        resolved_ips: resolved,
        target_ip: Some(ip),
        mode,
        tcp_port: (mode == PingMode::Tcp).then_some(tcp_port),
        sent,
        received,
        loss_percent,
        rtt_min_ms: min_ms,
        rtt_avg_ms: avg_ms,
        rtt_max_ms: max_ms,
        samples,
        note,
    }))
}

#[instrument(name = "net-debug-tcp", skip(_ctx, payload))]
async fn tcp_connect(
    _ctx: RequestContext,
    payload: Json<TcpConnectRequest>,
) -> WebResult<WebResponse<TcpConnectResponse>> {
    let payload = payload.into_inner();

    if payload.port == 0 {
        return Err(WebError::BadRequest("Invalid port".to_string()));
    }

    let timeout_ms: u64 = payload.timeout_ms.unwrap_or(3000).clamp(100, 60_000);
    let read_banner = payload.read_banner.unwrap_or(true);
    let banner_bytes: usize = payload.banner_bytes.unwrap_or(256).clamp(1, 4096);

    let resolved = resolve_and_validate_host(&payload.host, Some(payload.port)).await?;
    if resolved.is_empty() {
        return Err(WebError::BadRequest("Unable to resolve host".to_string()));
    }

    let mut last_err: Option<String> = None;
    for ip in resolved.iter().copied() {
        let start = Instant::now();
        let conn = timeout(
            Duration::from_millis(timeout_ms),
            TcpStream::connect((ip, payload.port)),
        )
        .await;

        match conn {
            Ok(Ok(stream)) => {
                let connect_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                let banner = if read_banner {
                    let mut buf = vec![0u8; banner_bytes];
                    match timeout(Duration::from_millis(500), stream.readable()).await {
                        Ok(Ok(())) => match stream.try_read(&mut buf) {
                            Ok(n) if n > 0 => Some(String::from_utf8_lossy(&buf[..n]).to_string()),
                            _ => None,
                        },
                        _ => None,
                    }
                } else {
                    None
                };

                return Ok(WebResponse::ok(TcpConnectResponse {
                    host: payload.host,
                    port: payload.port,
                    resolved_ips: resolved,
                    target_ip: Some(ip),
                    connected: true,
                    connect_ms: Some(connect_ms),
                    banner,
                    error: None,
                }));
            }
            Ok(Err(e)) => last_err = Some(e.to_string()),
            Err(_) => last_err = Some("TCP connect timeout".to_string()),
        }
    }

    Ok(WebResponse::ok(TcpConnectResponse {
        host: payload.host,
        port: payload.port,
        resolved_ips: resolved,
        target_ip: None,
        connected: false,
        connect_ms: None,
        banner: None,
        error: last_err.or(Some("Connect failed".to_string())),
    }))
}

#[instrument(name = "net-debug-http", skip(_ctx, payload))]
async fn http_request(
    _ctx: RequestContext,
    payload: Json<HttpRequest>,
) -> WebResult<WebResponse<HttpResponse>> {
    let payload = payload.into_inner();
    let timeout_ms: u64 = payload.timeout_ms.unwrap_or(8000).clamp(200, 120_000);
    let follow_redirects = payload.follow_redirects.unwrap_or(true);
    let insecure_tls = payload.insecure_tls.unwrap_or(false);
    let max_response_bytes: usize = payload
        .max_response_bytes
        .unwrap_or(256 * 1024)
        .clamp(1_024, 2 * 1024 * 1024);

    let url =
        Url::parse(&payload.url).map_err(|e| WebError::BadRequest(format!("Invalid URL: {e}")))?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(WebError::BadRequest(
            "Only http/https schemes are allowed".to_string(),
        ));
    }

    let host = url
        .host_str()
        .ok_or_else(|| WebError::BadRequest("Missing URL host".to_string()))?
        .to_string();
    let port = url.port_or_known_default();
    let resolved = resolve_and_validate_host(&host, port).await?;

    let start = Instant::now();

    let redirect_policy = if follow_redirects {
        reqwest::redirect::Policy::limited(5)
    } else {
        reqwest::redirect::Policy::none()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(redirect_policy)
        .danger_accept_invalid_certs(insecure_tls)
        .user_agent("ng-gateway/net-debug")
        .build()
        .map_err(|e| WebError::BadRequest(format!("HTTP client build failed: {e}")))?;

    let mut req = client.request(to_reqwest_method(payload.method), url.clone());

    if let Some(headers) = payload.headers {
        for (k, v) in headers {
            if k.trim().is_empty() {
                continue;
            }
            req = req.header(k, v);
        }
    }

    if let Some(body) = payload.body {
        // Do not attach body for GET/HEAD; let server decide.
        req = req.body(body);
    }

    let resp = req.send().await;

    let total_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    match resp {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let mut headers_map: BTreeMap<String, String> = BTreeMap::new();
            for (k, v) in resp.headers().iter() {
                headers_map.insert(
                    k.to_string(),
                    v.to_str().unwrap_or("<non-utf8>").to_string(),
                );
            }

            // Stream body with a hard limit.
            let mut body_bytes: Vec<u8> = Vec::new();
            let mut truncated = false;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|e| WebError::BadRequest(format!("Read body failed: {e}")))?;
                if body_bytes.len().saturating_add(chunk.len()) > max_response_bytes {
                    let remaining = max_response_bytes.saturating_sub(body_bytes.len());
                    body_bytes.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    break;
                }
                body_bytes.extend_from_slice(&chunk);
            }

            let body = if body_bytes.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&body_bytes).to_string())
            };

            Ok(WebResponse::ok(HttpResponse {
                url: payload.url,
                resolved_ips: resolved,
                status: Some(status),
                headers: headers_map,
                body,
                body_truncated: truncated,
                total_ms,
                error: None,
            }))
        }
        Err(e) => Ok(WebResponse::ok(HttpResponse {
            url: payload.url,
            resolved_ips: resolved,
            status: None,
            headers: BTreeMap::new(),
            body: None,
            body_truncated: false,
            total_ms,
            error: Some(e.to_string()),
        })),
    }
}
