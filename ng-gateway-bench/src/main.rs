mod metrics;
mod protocol;
mod publisher;
mod scenarios;
mod stats;

use anyhow::{anyhow, Context};
use clap::{Parser, ValueEnum};
use metrics::{sample_for, MetricsSummary};
use ng_gateway_sdk::{NGValue, NorthwardData};
use protocol::ChannelRuntime;
use rand::RngExt;
use scenarios::{Scenario, ScenarioKind, MAX_SCENARIO_ID};
use stats::{fmt_duration_ms, DurationStats};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::time::MissedTickBehavior;

// ───────────────────────────────────────────────────────────────────────────
// CLI
// ───────────────────────────────────────────────────────────────────────────

/// Benchmark tool for ng-gateway southward drivers (Modbus / OPC UA).
///
/// # Goals
/// - Reproduce realistic collection schedules (1s / 100ms).
/// - Measure end-to-end `collect_data` duration and payload size.
/// - Provide basic CPU / memory / network summaries.
/// - Measure downlink latency via driver `write_point` (scenario 7).
/// - Measure downlink latency via HTTP API `write_point` (scenario 8).
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    /// Protocol under test.
    ///
    /// NOTE:
    /// `ng-gateway-bench` links drivers as Rust dependencies. Under the workspace release profile
    /// (LTO enabled), linking multiple drivers into one binary will fail due to duplicate C-ABI
    /// symbols (e.g. `create_driver_factory`).
    ///
    /// Therefore we only support running **one** protocol per built binary.
    ///
    /// For scenario 8 (API-only) the protocol is informational only (appears in output tables).
    #[arg(long, value_enum, default_value = "modbus")]
    protocol: ProtocolOpt,

    /// Scenario id (1..=8).
    ///
    /// Use `--all-scenarios` to run all applicable scenarios.
    #[arg(long)]
    scenario: Option<u8>,

    /// Run all scenarios.
    ///
    /// Scenarios 1–7 always run.  Scenario 8 is included only when `--api-base-url` is provided.
    #[arg(long, default_value_t = false)]
    all_scenarios: bool,

    /// Warmup seconds before recording stats (scenarios 1–7).
    #[arg(long, default_value_t = 5)]
    warmup_secs: u64,

    /// Measurement seconds (scenarios 1–7).
    #[arg(long, default_value_t = 20)]
    duration_secs: u64,

    /// System sampler interval in milliseconds (scenarios 1–7).
    #[arg(long, default_value_t = 500)]
    sample_interval_ms: u64,

    // ──────────── Modbus defaults ────────────
    #[arg(long, default_value = "8.155.153.52")]
    modbus_host: String,
    #[arg(long, default_value_t = 502)]
    modbus_port: u16,
    #[arg(long, default_value_t = 1)]
    modbus_slave_id: u8,
    /// Maximum Modbus slave id used when generating per-device slave ids.
    #[arg(long, default_value_t = 10)]
    modbus_slave_id_max: u8,
    /// Base register address.
    #[arg(long, default_value_t = 0)]
    modbus_address_base: u16,
    /// Address step per Float32 point (default 2 => 2 registers per float).
    #[arg(long, default_value_t = 2)]
    modbus_address_step: u16,
    /// TCP pool size per channel.
    #[arg(long, default_value_t = 10)]
    modbus_tcp_pool_size: u16,

    // ──────────── OPC UA defaults ────────────
    #[arg(
        long,
        default_value = "opc.tcp://192.168.66.8:53530/OPCUA/SimulationServer"
    )]
    opcua_endpoint: String,
    #[arg(long, default_value = "SimulationServer@shiyuecamus-MacBook-Pro")]
    opcua_application_name: String,
    #[arg(
        long,
        default_value = "urn:shiyuecamus-MacBook-Pro.local:0PCUA:SimulationServer"
    )]
    opcua_application_uri: String,
    /// NodeId start for `ns=3;i=...`
    #[arg(long, default_value_t = 1002)]
    opcua_node_id_start: u32,

    // ──────────── Driver downlink (scenario 7) ────────────
    /// Downlink point count for scenario 7.
    #[arg(long, default_value_t = 100)]
    downlink_points: usize,
    /// Downlink test iterations for scenario 7.
    #[arg(long, default_value_t = 50)]
    downlink_iterations: usize,
    /// Downlink timeout in milliseconds (per write_point call).
    #[arg(long, default_value_t = 3000)]
    downlink_timeout_ms: u64,

    // ──────────── API downlink (scenario 8) ────────────
    /// Gateway API base URL for scenario 8 (API write-point).
    ///
    /// Required for scenario 8.  The gateway must be running and reachable.
    /// Example: `http://192.168.1.11:8978`
    #[arg(long)]
    api_base_url: Option<String>,
    /// Username for gateway API login.
    #[arg(long, default_value = "system_admin")]
    api_username: String,
    /// Password for gateway API login.
    #[arg(long, default_value = "system_admin")]
    api_password: String,
    /// API version header value.
    #[arg(long, default_value = "v1")]
    api_version: String,
    /// Start device ID for API downlink (inclusive).
    #[arg(long, default_value_t = 1)]
    api_device_id_start: i32,
    /// End device ID for API downlink (inclusive).
    #[arg(long, default_value_t = 100)]
    api_device_id_end: i32,
    /// Point key prefix for API downlink (e.g. "p" → keys "p1"…"p1000").
    #[arg(long, default_value = "p")]
    api_point_key_prefix: String,
    /// Points per device for API downlink.
    #[arg(long, default_value_t = 1000)]
    api_points_per_device: usize,
    /// API downlink test iterations (scenario 8).
    #[arg(long, default_value_t = 100)]
    api_downlink_iterations: usize,
    /// API downlink timeout per write in milliseconds.
    #[arg(long, default_value_t = 3000)]
    api_downlink_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProtocolOpt {
    Modbus,
    Opcua,
}

impl ProtocolOpt {
    fn label(self) -> &'static str {
        match self {
            Self::Modbus => "modbus",
            Self::Opcua => "opcua",
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Main
// ───────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let cli = Cli::parse();
    let mut rows: Vec<BenchRow> = Vec::new();

    // Resolve scenario set.
    let scenarios: Vec<Scenario> = if cli.all_scenarios {
        (1u8..=MAX_SCENARIO_ID)
            .filter_map(|id| {
                let s = Scenario::from_id(id)?;
                // Scenario 8 requires API base URL; skip silently if not configured.
                if s.kind == ScenarioKind::ApiWritePoint && cli.api_base_url.is_none() {
                    tracing::info!("Skipping scenario {id} (--api-base-url not set)");
                    return None;
                }
                Some(s)
            })
            .collect()
    } else {
        let id = cli.scenario.ok_or(anyhow!(
            "missing --scenario (1..={MAX_SCENARIO_ID}) or set --all-scenarios"
        ))?;
        vec![Scenario::from_id(id).ok_or(anyhow!("invalid scenario id: {id}"))?]
    };

    // Pre-authenticate if any API scenario is requested (reuse token).
    let need_api = scenarios
        .iter()
        .any(|s| s.kind == ScenarioKind::ApiWritePoint);
    let api_client = if need_api {
        Some(build_api_client(&cli).await?)
    } else {
        None
    };

    // Run all requested scenarios sequentially.
    for scenario in scenarios.iter() {
        let row = match scenario.kind {
            ScenarioKind::Collect | ScenarioKind::CollectAndDownlink => {
                run_driver_scenario(&cli, scenario, cli.protocol).await?
            }
            ScenarioKind::ApiWritePoint => {
                let client = api_client
                    .as_ref()
                    .ok_or(anyhow!("--api-base-url is required for scenario 8"))?;
                run_api_scenario(&cli, scenario, client).await?
            }
        };
        rows.push(row);
    }

    // ── Output ──
    let has_collect = rows.iter().any(|r| r.collect.is_some());
    let has_downlink = rows.iter().any(|r| r.downlink.is_some());

    if has_collect {
        print_collect_table(&rows);
    }
    if has_downlink {
        print_downlink_table(&rows);
    }

    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// Scenario runners
// ───────────────────────────────────────────────────────────────────────────

/// Scenarios 1–7: build local channel runtimes, collect, optionally write.
async fn run_driver_scenario(
    cli: &Cli,
    scenario: &Scenario,
    protocol: ProtocolOpt,
) -> anyhow::Result<BenchRow> {
    let warmup = Duration::from_secs(cli.warmup_secs);
    let duration = Duration::from_secs(cli.duration_secs);
    let sample_interval = Duration::from_millis(cli.sample_interval_ms.max(1));

    let channels = build_channels(cli, scenario, protocol).context("build_channels")?;
    for ch in channels.iter() {
        ch.start().await.context("driver.start")?;
    }

    // Warmup phase (no stats).
    run_collect_phase(&channels, scenario.period_ms, warmup, None).await?;

    // Measurement phase.
    let collect_fut = run_collect_phase(&channels, scenario.period_ms, duration, Some("measure"));
    let metrics_fut = sample_for(duration, sample_interval);

    let (_collect_stats, _point_count, _cycle_count, metrics, downlink) = match scenario.kind {
        ScenarioKind::Collect => {
            let ((s, p, c), m) = tokio::try_join!(collect_fut, metrics_fut)?;
            (s, p, c, m, None)
        }
        ScenarioKind::CollectAndDownlink => {
            let downlink_fut = async {
                // Let collectors enter steady state before measuring downlink.
                tokio::time::sleep(Duration::from_secs(1)).await;
                run_driver_downlink_phase(
                    &channels,
                    cli.downlink_points,
                    cli.downlink_iterations,
                    cli.downlink_timeout_ms,
                )
                .await
            };
            let ((s, p, c), m, d) = tokio::try_join!(collect_fut, metrics_fut, downlink_fut)?;
            (s, p, c, m, Some(d))
        }
        // ApiWritePoint handled in run_api_scenario.
        ScenarioKind::ApiWritePoint => unreachable!(),
    };

    for ch in channels.iter() {
        let _ = ch.stop().await;
    }

    Ok(BenchRow {
        protocol: protocol.label().to_string(),
        scenario_id: scenario.id,
        collect: Some(CollectInfo {
            channel_count: scenario.channel_count,
            devices_per_channel: scenario.devices_per_channel,
            points_per_device: scenario.points_per_device,
            period_ms: scenario.period_ms,
            total_points: scenario.total_points,
            point_type: "Float32".to_string(),
            avg_cpu_pct: metrics.avg_process_cpu_pct as f64,
            peak_rss_bytes: metrics.peak_process_rss_bytes,
            net_rx_bps: metrics.avg_net_rx_bps,
            net_tx_bps: metrics.avg_net_tx_bps,
        }),
        downlink,
    })
}

/// Scenario 8: pure HTTP API write-point latency test.
async fn run_api_scenario(
    cli: &Cli,
    scenario: &Scenario,
    api_client: &ApiClient,
) -> anyhow::Result<BenchRow> {
    let downlink = run_api_downlink_phase(
        api_client,
        cli.api_device_id_start,
        cli.api_device_id_end,
        &cli.api_point_key_prefix,
        cli.api_points_per_device,
        cli.api_downlink_iterations,
        cli.api_downlink_timeout_ms,
    )
    .await?;

    Ok(BenchRow {
        protocol: cli.protocol.label().to_string(),
        scenario_id: scenario.id,
        collect: None,
        downlink: Some(downlink),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Channel builder
// ───────────────────────────────────────────────────────────────────────────

fn build_channels(
    cli: &Cli,
    scenario: &Scenario,
    protocol: ProtocolOpt,
) -> anyhow::Result<Vec<Arc<ChannelRuntime>>> {
    let mut channels = Vec::with_capacity(scenario.channel_count);
    for ch_idx in 0..scenario.channel_count {
        let ch = match protocol {
            ProtocolOpt::Modbus => {
                #[cfg(feature = "modbus")]
                {
                    protocol::modbus::build_modbus_channel_runtime(
                        protocol::modbus::ModbusChannelRuntimeArgs {
                            channel_idx: ch_idx,
                            devices_per_channel: scenario.devices_per_channel,
                            points_per_device: scenario.points_per_device,
                            period_ms: scenario.period_ms,
                            host: cli.modbus_host.clone(),
                            port: cli.modbus_port,
                            slave_id_base: cli.modbus_slave_id,
                            slave_id_max: cli.modbus_slave_id_max,
                            address_base: cli.modbus_address_base,
                            address_step: cli.modbus_address_step,
                            tcp_pool_size: cli.modbus_tcp_pool_size,
                        },
                    )
                    .map_err(|e| anyhow!(e))?
                }
                #[cfg(not(feature = "modbus"))]
                {
                    return Err(anyhow!(
                        "Modbus benchmark is not enabled. Rebuild with `--features modbus`."
                    ));
                }
            }
            ProtocolOpt::Opcua => {
                #[cfg(feature = "opcua")]
                {
                    protocol::opcua::build_opcua_channel_runtime(
                        protocol::opcua::OpcuaChannelRuntimeArgs {
                            channel_idx: ch_idx,
                            devices_per_channel: scenario.devices_per_channel,
                            points_per_device: scenario.points_per_device,
                            period_ms: scenario.period_ms,
                            endpoint_url: cli.opcua_endpoint.clone(),
                            application_name: cli.opcua_application_name.clone(),
                            application_uri: cli.opcua_application_uri.clone(),
                            node_id_start: cli.opcua_node_id_start,
                        },
                    )
                    .map_err(|e| anyhow!(e))?
                }
                #[cfg(not(feature = "opcua"))]
                {
                    return Err(anyhow!(
                        "OPC UA benchmark is not enabled. Rebuild with `--features opcua`."
                    ));
                }
            }
        };
        channels.push(Arc::new(ch));
    }
    Ok(channels)
}

// ───────────────────────────────────────────────────────────────────────────
// Collection phase
// ───────────────────────────────────────────────────────────────────────────

/// Run collection tasks for all channels for the given duration.
///
/// Returns: (duration_stats, collected_point_values, cycles_executed_total).
async fn run_collect_phase(
    channels: &[Arc<ChannelRuntime>],
    period_ms: u32,
    duration: Duration,
    _label: Option<&'static str>,
) -> anyhow::Result<(DurationStats, usize, u64)> {
    let period = Duration::from_millis(period_ms.max(1) as u64);
    let end_at = Instant::now() + duration;

    let mut handles = Vec::with_capacity(channels.len());
    for ch in channels.iter() {
        let ch = Arc::clone(ch);
        handles.push(tokio::spawn(async move {
            let mut stats = DurationStats::default();
            let mut collected_points: usize = 0;
            let mut cycles: u64 = 0;

            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            while Instant::now() < end_at {
                ticker.tick().await;
                let t0 = Instant::now();
                let res = ch.collect_once().await;
                let elapsed = t0.elapsed();

                if let Ok(list) = res {
                    collected_points = collected_points.saturating_add(count_point_values(&list));
                }
                stats.record(elapsed);
                cycles = cycles.saturating_add(1);
            }

            Ok::<_, anyhow::Error>((stats, collected_points, cycles))
        }));
    }

    let mut agg = DurationStats::default();
    let mut points: usize = 0;
    let mut cycles: u64 = 0;

    for h in handles {
        let (s, p, c) = h.await.context("join collect task")??;
        agg.merge_from(&s);
        points = points.saturating_add(p);
        cycles = cycles.saturating_add(c);
    }

    Ok((agg, points, cycles))
}

fn count_point_values(list: &[NorthwardData]) -> usize {
    list.iter()
        .map(|d| match d {
            NorthwardData::Telemetry(t) => t.values.len(),
            NorthwardData::Attributes(a) => {
                a.client_attributes.len() + a.shared_attributes.len() + a.server_attributes.len()
            }
            _ => 0,
        })
        .sum()
}

// ───────────────────────────────────────────────────────────────────────────
// Driver downlink phase (scenario 7)
// ───────────────────────────────────────────────────────────────────────────

async fn run_driver_downlink_phase(
    channels: &[Arc<ChannelRuntime>],
    downlink_points: usize,
    iterations: usize,
    timeout_ms: u64,
) -> anyhow::Result<DownlinkStats> {
    let ch = channels
        .first()
        .ok_or(anyhow!("no channels available for downlink"))?;
    let device = ch
        .devices
        .first()
        .cloned()
        .ok_or(anyhow!("no devices available for downlink"))?;

    let points: Vec<_> = ch
        .downlink_points
        .iter()
        .take(downlink_points.max(1))
        .cloned()
        .collect();

    if points.is_empty() {
        return Err(anyhow!("no downlink points available"));
    }

    let timeout = Some(Duration::from_millis(timeout_ms.max(1)));
    let mut stats = DurationStats::default();
    let mut ok: u64 = 0;
    let mut fail: u64 = 0;

    for i in 0..iterations.max(1) {
        let p = points[i % points.len()].clone();
        let value = NGValue::Float32(123.456);
        let t0 = Instant::now();
        let res = ch
            .write_point_once(Arc::clone(&device), p, value, timeout)
            .await;
        let elapsed = t0.elapsed();
        stats.record(elapsed);
        match res {
            Ok(_) => ok += 1,
            Err(_) => fail += 1,
        }
    }

    Ok(DownlinkStats {
        mode: "driver write_point".to_string(),
        device_range: None,
        point_count: points.len(),
        iterations: iterations.max(1) as u64,
        ok,
        fail,
        min: stats.min(),
        max: stats.max(),
        avg: stats.avg(),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// API downlink phase (scenario 8)
// ───────────────────────────────────────────────────────────────────────────

/// Authenticated HTTP client for gateway API calls.
struct ApiClient {
    client: reqwest::Client,
    /// Full URL for the write-point endpoint.
    write_url: String,
    /// Bearer token obtained from login.
    token: String,
    /// API version header value.
    api_version: String,
}

/// Login response shape (only the fields we need).
#[derive(serde::Deserialize)]
struct LoginApiResponse {
    code: u16,
    data: Option<LoginData>,
    message: String,
}

#[derive(serde::Deserialize)]
struct LoginData {
    token: String,
}

/// Build an authenticated API client by logging into the gateway.
async fn build_api_client(cli: &Cli) -> anyhow::Result<ApiClient> {
    let base = cli
        .api_base_url
        .as_deref()
        .ok_or(anyhow!("--api-base-url is required for API downlink"))?;
    let base = base.trim_end_matches('/');

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build reqwest client")?;

    let login_url = format!("{base}/api/auth/login");
    tracing::info!("Logging into gateway API at {login_url}");

    let resp = client
        .post(&login_url)
        .header("X-API-Version", cli.api_version.as_str())
        .json(&serde_json::json!({
            "username": cli.api_username,
            "password": cli.api_password,
        }))
        .send()
        .await
        .context("API login request failed")?;

    let status = resp.status();
    let body: LoginApiResponse = resp
        .json()
        .await
        .context("failed to parse login response")?;

    if !status.is_success() || body.code != 0 {
        return Err(anyhow!(
            "API login failed (HTTP {}): {}",
            status,
            body.message
        ));
    }

    let token = body
        .data
        .ok_or(anyhow!("login response missing data"))?
        .token;

    tracing::info!("API login successful, token acquired");

    Ok(ApiClient {
        client,
        write_url: format!("{base}/api/point/write"),
        token,
        api_version: cli.api_version.clone(),
    })
}

/// Generic API response shape (we only inspect `code` and `message`).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WritePointApiResponse {
    code: u16,
    message: String,
}

/// Run API-based downlink phase: send random write-point requests via HTTP.
///
/// Each iteration picks a random device and random point within the configured
/// ranges, sends a `POST /api/point/write`, and records the round-trip latency.
async fn run_api_downlink_phase(
    api: &ApiClient,
    device_id_start: i32,
    device_id_end: i32,
    point_key_prefix: &str,
    points_per_device: usize,
    iterations: usize,
    timeout_ms: u64,
) -> anyhow::Result<DownlinkStats> {
    let iterations = iterations.max(1);
    let device_count = (device_id_end - device_id_start + 1).max(1);
    let points_count = points_per_device.max(1);

    let mut stats = DurationStats::default();
    let mut ok: u64 = 0;
    let mut fail: u64 = 0;
    let mut rng = rand::rng();

    tracing::info!(
        "Starting API downlink: {} iterations, devices {}..={}, points {prefix}1..{prefix}{pts}",
        iterations,
        device_id_start,
        device_id_end,
        prefix = point_key_prefix,
        pts = points_count,
    );

    for i in 0..iterations {
        // Random device and point selection.
        let device_id = device_id_start + rng.random_range(0..device_count);
        let point_idx = rng.random_range(1..=points_count as i32);
        let point_key = format!("{}{}", point_key_prefix, point_idx);
        let value: f32 = rng.random_range(0.0f32..1000.0f32);

        let payload = serde_json::json!({
            "deviceId": device_id,
            "pointKey": point_key,
            "value": value,
            "timeoutMs": timeout_ms,
        });

        let t0 = Instant::now();
        let result = api
            .client
            .post(&api.write_url)
            .header("Authorization", format!("Bearer {}", api.token))
            .header("X-API-Version", api.api_version.as_str())
            .json(&payload)
            .send()
            .await;
        let elapsed = t0.elapsed();
        stats.record(elapsed);

        match result {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.json::<WritePointApiResponse>().await {
                        Ok(body) if body.code == 0 => ok += 1,
                        Ok(body) => {
                            fail += 1;
                            if i < 3 {
                                tracing::warn!(
                                    "write_point API error (code {}): {}",
                                    body.code,
                                    body.message
                                );
                            }
                        }
                        Err(e) => {
                            fail += 1;
                            if i < 3 {
                                tracing::warn!("failed to parse write_point response: {e}");
                            }
                        }
                    }
                } else {
                    fail += 1;
                    if i < 3 {
                        let body = resp.text().await.unwrap_or_default();
                        tracing::warn!("write_point HTTP {status}: {body}");
                    }
                }
            }
            Err(e) => {
                fail += 1;
                if i < 3 {
                    tracing::warn!("write_point request failed: {e}");
                }
            }
        }
    }

    tracing::info!(
        "API downlink completed: ok={ok}, fail={fail}, avg={:?}",
        stats.avg()
    );

    Ok(DownlinkStats {
        mode: "API write_point".to_string(),
        device_range: Some(format!("{}..={}", device_id_start, device_id_end)),
        point_count: points_count,
        iterations: iterations as u64,
        ok,
        fail,
        min: stats.min(),
        max: stats.max(),
        avg: stats.avg(),
    })
}

// ───────────────────────────────────────────────────────────────────────────
// Data models
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DownlinkStats {
    mode: String,
    /// Optional device range label (e.g. "1..=100") for API scenarios.
    device_range: Option<String>,
    point_count: usize,
    iterations: u64,
    ok: u64,
    fail: u64,
    min: Option<Duration>,
    max: Option<Duration>,
    avg: Option<Duration>,
}

/// Collection performance info (scenarios 1–7).
#[derive(Debug, Clone)]
struct CollectInfo {
    channel_count: usize,
    devices_per_channel: usize,
    points_per_device: usize,
    period_ms: u32,
    total_points: usize,
    point_type: String,
    avg_cpu_pct: f64,
    peak_rss_bytes: u64,
    net_rx_bps: f64,
    net_tx_bps: f64,
}

#[derive(Debug, Clone)]
struct BenchRow {
    protocol: String,
    scenario_id: u8,
    /// Present for scenarios 1–7 (driver-based collection).
    collect: Option<CollectInfo>,
    /// Present for scenarios 7 & 8 (downlink / API write).
    downlink: Option<DownlinkStats>,
}

// ───────────────────────────────────────────────────────────────────────────
// Output
// ───────────────────────────────────────────────────────────────────────────

fn print_collect_table(rows: &[BenchRow]) {
    let collect_rows: Vec<_> = rows.iter().filter(|r| r.collect.is_some()).collect();
    if collect_rows.is_empty() {
        return;
    }

    println!();
    println!("数据采集性能测试（Markdown 表格）");
    println!();
    println!("| 场景 | 协议 | Channel数量 | 每个Channel设备数 | 每个设备点位数 | 采集频率 | 总计点位 | 点位类型 | 内存使用(peak RSS) | CPU 使用(avg) | 网络带宽消耗 |");
    println!("|---:|---|---:|---:|---:|---|---:|---|---|---|---|");

    for r in collect_rows {
        let c = r.collect.as_ref().unwrap();
        let mem = MetricsSummary::fmt_mib(c.peak_rss_bytes);
        let cpu = format!("{:.2}%", c.avg_cpu_pct);
        let rx = MetricsSummary::fmt_kb_per_sec(c.net_rx_bps);
        let tx = MetricsSummary::fmt_kb_per_sec(c.net_tx_bps);
        let bw = format!("receive：{} transmit：{}", rx, tx);
        let freq = format!("{} ms", c.period_ms);
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.scenario_id,
            r.protocol,
            c.channel_count,
            c.devices_per_channel,
            c.points_per_device,
            freq,
            c.total_points,
            c.point_type,
            mem,
            cpu,
            bw
        );
    }
}

fn print_downlink_table(rows: &[BenchRow]) {
    let dl_rows: Vec<_> = rows.iter().filter(|r| r.downlink.is_some()).collect();
    if dl_rows.is_empty() {
        return;
    }

    println!();
    println!("数据下发延迟测试（Markdown 表格）");
    println!();
    println!("| 场景 | 协议 | 下发方式 | 设备范围 | 点位数/设备 | 测试次数 | 成功 | 失败 | 最小响应时间 | 最大响应时间 | 平均响应时间 |");
    println!("|---:|---|---|---|---:|---:|---:|---:|---|---|---|");

    for r in dl_rows {
        let d = r.downlink.as_ref().unwrap();
        let device_range = d.device_range.as_deref().unwrap_or("-");
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.scenario_id,
            r.protocol,
            d.mode,
            device_range,
            d.point_count,
            d.iterations,
            d.ok,
            d.fail,
            fmt_duration_ms(d.min),
            fmt_duration_ms(d.max),
            fmt_duration_ms(d.avg)
        );
    }
}
