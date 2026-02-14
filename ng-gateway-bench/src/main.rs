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
use scenarios::{Scenario, ScenarioKind};
use stats::{fmt_duration_ms, DurationStats};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::time::MissedTickBehavior;

/// Benchmark tool for ng-gateway southward drivers (Modbus / OPC UA).
///
/// # Goals
/// - Reproduce realistic collection schedules (1s / 100ms).
/// - Measure end-to-end `collect_data` duration and payload size.
/// - Provide basic CPU / memory / network summaries.
/// - Measure downlink latency via `write_point`.
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
    #[arg(long, value_enum, default_value = "modbus")]
    protocol: ProtocolOpt,

    /// Scenario id (1..=7).
    ///
    /// Use `--all-scenarios` to run all scenarios.
    #[arg(long)]
    scenario: Option<u8>,

    /// Run all scenarios (1..=7).
    #[arg(long, default_value_t = false)]
    all_scenarios: bool,

    /// Warmup seconds before recording stats.
    #[arg(long, default_value_t = 5)]
    warmup_secs: u64,

    /// Measurement seconds.
    #[arg(long, default_value_t = 20)]
    duration_secs: u64,

    /// System sampler interval in milliseconds.
    #[arg(long, default_value_t = 500)]
    sample_interval_ms: u64,

    // ---------------- Modbus defaults (from your environment) ----------------
    #[arg(long, default_value = "8.155.153.52")]
    modbus_host: String,
    #[arg(long, default_value_t = 502)]
    modbus_port: u16,
    #[arg(long, default_value_t = 1)]
    modbus_slave_id: u8,
    /// Maximum Modbus slave id used when generating per-device slave ids.
    ///
    /// Why:
    /// - Benchmark scenarios may create many devices across many channels.
    /// - Using a single fixed slave id (e.g. always 1) is unrealistic when your simulator
    ///   already provisions multiple slave units (e.g. 1..=10).
    ///
    /// Behavior:
    /// - For each device, we assign a slave id in the range `[modbus_slave_id, modbus_slave_id_max]`
    ///   in a round-robin fashion using a global device index across channels.
    /// - Set this to `1` to keep legacy behavior (all devices use slave id 1).
    #[arg(long, default_value_t = 10)]
    modbus_slave_id_max: u8,
    /// Base register address.
    #[arg(long, default_value_t = 0)]
    modbus_address_base: u16,
    /// Address step per Float32 point (default 2 => 2 registers per float).
    #[arg(long, default_value_t = 2)]
    modbus_address_step: u16,
    /// TCP pool size per channel (default 1).
    #[arg(long, default_value_t = 10)]
    modbus_tcp_pool_size: u16,

    // ---------------- OPC UA defaults (from your environment) ----------------
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

    // ---------------- Downlink config ----------------
    /// Downlink point count for scenario 7.
    #[arg(long, default_value_t = 100)]
    downlink_points: usize,
    /// Downlink test iterations for scenario 7.
    #[arg(long, default_value_t = 50)]
    downlink_iterations: usize,
    /// Downlink timeout in milliseconds (per write_point call).
    #[arg(long, default_value_t = 3000)]
    downlink_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProtocolOpt {
    Modbus,
    Opcua,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let cli = Cli::parse();
    let mut rows: Vec<BenchRow> = Vec::new();

    // Resolve scenario set.
    let scenarios: Vec<Scenario> = if cli.all_scenarios {
        (1u8..=7u8).filter_map(Scenario::from_id).collect()
    } else {
        let id = cli
            .scenario
            .ok_or(anyhow!("missing --scenario (1..=7) or set --all-scenarios"))?;
        vec![Scenario::from_id(id).ok_or(anyhow!("invalid scenario id: {}", id))?]
    };

    // Run all requested scenarios sequentially (stable + avoids cross-scenario interference).
    for scenario in scenarios.iter() {
        match cli.protocol {
            ProtocolOpt::Modbus => {
                let row = run_for_protocol(&cli, scenario, ProtocolOpt::Modbus).await?;
                rows.push(row);
            }
            ProtocolOpt::Opcua => {
                let row = run_for_protocol(&cli, scenario, ProtocolOpt::Opcua).await?;
                rows.push(row);
            }
        }
    }

    print_collect_table(&rows);

    if rows.iter().any(|r| r.downlink.is_some()) {
        print_downlink_table(&rows);
    }

    Ok(())
}

async fn run_for_protocol(
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

    // Measurement phase (collection + optional downlink + system metrics).
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
                run_downlink_phase(
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
    };

    for ch in channels.iter() {
        let _ = ch.stop().await;
    }

    Ok(BenchRow {
        protocol: match protocol {
            ProtocolOpt::Modbus => "modbus",
            ProtocolOpt::Opcua => "opcua",
        }
        .to_string(),
        scenario_id: scenario.id,
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
        downlink,
    })
}

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
        // Merge stats (full-fidelity).
        agg.merge_from(&s);
        points = points.saturating_add(p);
        cycles = cycles.saturating_add(c);
    }

    Ok((agg, points, cycles))
}

async fn run_downlink_phase(
    channels: &[Arc<ChannelRuntime>],
    downlink_points: usize,
    iterations: usize,
    timeout_ms: u64,
) -> anyhow::Result<DownlinkStats> {
    // Use first channel and first device for downlink tests.
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
            Ok(_) => ok = ok.saturating_add(1),
            Err(_) => fail = fail.saturating_add(1),
        }
    }

    Ok(DownlinkStats {
        mode: "write_point".to_string(),
        point_count: points.len(),
        iterations: iterations.max(1) as u64,
        ok,
        fail,
        min: stats.min(),
        max: stats.max(),
        avg: stats.avg(),
    })
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

#[derive(Debug, Clone)]
struct DownlinkStats {
    mode: String,
    point_count: usize,
    iterations: u64,
    ok: u64,
    fail: u64,
    min: Option<Duration>,
    max: Option<Duration>,
    avg: Option<Duration>,
}

#[derive(Debug, Clone)]
struct BenchRow {
    protocol: String,
    scenario_id: u8,
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

    downlink: Option<DownlinkStats>,
}

fn print_collect_table(rows: &[BenchRow]) {
    println!();
    println!("数据采集性能测试（Markdown 表格）");
    println!();
    println!("| 场景 | 协议 | Channel数量 | 每个Channel设备数 | 每个设备点位数 | 采集频率 | 总计点位 | 点位类型 | 内存使用(peak RSS) | CPU 使用(avg) | 网络带宽消耗 |");
    println!("|---:|---|---:|---:|---:|---|---:|---|---|---|---|");

    for r in rows {
        let mem = MetricsSummary::fmt_mib(r.peak_rss_bytes);
        let cpu = format!("{:.2}%", r.avg_cpu_pct);
        let rx = MetricsSummary::fmt_kb_per_sec(r.net_rx_bps);
        let tx = MetricsSummary::fmt_kb_per_sec(r.net_tx_bps);
        let bw = format!("receive：{} transmit：{}", rx, tx);
        let freq = format!("{} ms", r.period_ms);
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.scenario_id,
            r.protocol,
            r.channel_count,
            r.devices_per_channel,
            r.points_per_device,
            freq,
            r.total_points,
            r.point_type,
            mem,
            cpu,
            bw
        );
    }
}

fn print_downlink_table(rows: &[BenchRow]) {
    println!();
    println!("数据下发延迟测试（Markdown 表格）");
    println!();
    println!("| 场景 | 协议 | 下发方式 | 下发点位数 | 测试次数 | 成功 | 失败 | 最小响应时间 | 最大响应时间 | 平均响应时间 |");
    println!("|---:|---|---|---:|---:|---:|---:|---|---|---|");

    for r in rows {
        let Some(d) = r.downlink.as_ref() else {
            continue;
        };
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.scenario_id,
            r.protocol,
            d.mode,
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
