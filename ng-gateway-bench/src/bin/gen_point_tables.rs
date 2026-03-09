use anyhow::Context;
use clap::Parser;
use ng_gateway_sdk::{DriverSchemas, FlattenEntity, TemplateMetadata, UiDataType};
use rust_xlsxwriter::{workbook::Workbook, Color, Format, FormatPattern};
use serde_json::Value as Json;
use std::{
    collections::HashMap,
    ffi::CStr,
    fs,
    os::raw::c_char,
    path::{Path, PathBuf},
    ptr, slice,
    time::Instant,
};

// Link exactly one driver crate to provide the FFI symbols.
// See ng-gateway-bench/Cargo.toml feature notes about duplicate symbols.
#[cfg(feature = "modbus")]
use ng_driver_modbus as _;
#[cfg(feature = "opcua")]
use ng_driver_opcua as _;

/// Generate benchmark Excel point tables for `FlattenEntity::DevicePoints`.
///
/// # Why this exists
/// - Import/preview performance benchmarks rely on a **real** gateway template shape:
///   - First worksheet: localized headers (labels)
///   - Hidden `__meta__` sheet: driver_type/entity/locale/schema_version
/// - The gateway parses **by header labels**, so we must generate headers from `DriverSchemas`.
///
/// # Output
/// For each scenario and each channel, it produces:
/// `{driver}-scenario{S}-channel{N}-device-points.xlsx`
///
/// Note:
/// - The generated file is an **XLSX** workbook (zip/XML).
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    /// Output directory.
    #[arg(long, default_value = "generated")]
    out_dir: PathBuf,

    /// Locale used for template headers, e.g. "zh-CN" or "en-US".
    #[arg(long, default_value = "zh-CN")]
    locale: String,

    /// Generate all built-in scenarios (1..=7).
    ///
    /// This uses the same presets described in benchmark docs, unless you override
    /// `--channels/--devices-per-channel/--points-per-device`.
    #[arg(long)]
    all: bool,

    /// Scenario id(s). Built-in scenarios are defined in docs under benchmark pages.
    ///
    /// When omitted, you must set `channels/devices_per_channel/points_per_device` manually.
    #[arg(long, value_delimiter = ',')]
    scenarios: Vec<u8>,

    /// Override channels count (takes precedence over scenario presets).
    #[arg(long)]
    channels: Option<u32>,

    /// Override devices per channel (takes precedence over scenario presets).
    #[arg(long)]
    devices_per_channel: Option<u32>,

    /// Override points per device (takes precedence over scenario presets).
    #[arg(long)]
    points_per_device: Option<u32>,

    /// Modbus: starting address for generated points.
    #[arg(long, default_value_t = 0)]
    modbus_base_address: u32,

    /// Modbus: maximum slave id to use when assigning per-device slave ids.
    ///
    /// Why:
    /// - Some benchmarks use multiple channels with 1 device each (e.g. scenario 6: 10 channels).
    /// - If we only use the device index **within a channel**, every channel's first device would
    ///   get slaveId=1.
    /// - This option lets us distribute slave ids across channels (default 1..=10).
    ///
    /// Notes:
    /// - The Modbus driver metadata validates slaveId in [1, 247].
    #[arg(long, default_value_t = 10)]
    modbus_slave_id_max: u32,

    /// Modbus: address stride between points.
    ///
    /// For `Float32` we typically use 2 registers, so default stride is 2.
    #[arg(long, default_value_t = 2)]
    modbus_address_stride: u32,

    /// OPC UA: NodeId template for generated points.
    ///
    /// Available placeholders:
    /// - `{channel}`: 1-based channel index
    /// - `{device}`:  1-based device index in the channel
    /// - `{point}`:   1-based point index in the device
    /// - `{ns}`:      namespace index (see `--opcua-nodeid-ns`)
    /// - `{node_i}`:  numeric node id, `opcua_nodeid_start + (point-1)` (see `--opcua-nodeid-start`)
    #[arg(long, default_value = "ns={ns};i={node_i}")]
    opcua_nodeid_template: String,

    /// OPC UA: default namespace index used by `{ns}` placeholder.
    #[arg(long, default_value_t = 3)]
    opcua_nodeid_ns: u16,

    /// OPC UA: start integer id used by `{node_i}` placeholder.
    #[arg(long, default_value_t = 1002)]
    opcua_nodeid_start: u32,
}

/// Built-in benchmark scenario preset.
#[derive(Debug, Clone, Copy)]
struct ScenarioPreset {
    channels: u32,
    devices_per_channel: u32,
    points_per_device: u32,
}

impl ScenarioPreset {
    /// Resolve built-in preset by scenario id.
    fn get(id: u8) -> Option<Self> {
        // Keep this aligned with docs:
        // - ng-gateway-ui/docs/src/guide/benchmark/modbus.md
        // - ng-gateway-ui/docs/src/guide/benchmark/opcua.md
        let preset = match id {
            1 => Self {
                channels: 1,
                devices_per_channel: 10,
                points_per_device: 1000,
            },
            2 => Self {
                channels: 5,
                devices_per_channel: 10,
                points_per_device: 1000,
            },
            3 => Self {
                channels: 10,
                devices_per_channel: 10,
                points_per_device: 1000,
            },
            4 => Self {
                channels: 1,
                devices_per_channel: 1,
                points_per_device: 1000,
            },
            5 => Self {
                channels: 5,
                devices_per_channel: 1,
                points_per_device: 1000,
            },
            6 => Self {
                channels: 10,
                devices_per_channel: 1,
                points_per_device: 1000,
            },
            7 => Self {
                channels: 10,
                devices_per_channel: 10,
                points_per_device: 1000,
            },
            _ => return None,
        };
        Some(preset)
    }
}

// FFI symbols exported by exactly one linked driver (see `ng-gateway-bench` feature flags).
extern "C" {
    fn ng_driver_metadata_json_ptr(out_ptr: *mut *const u8, out_len: *mut usize);
    fn ng_driver_type() -> *const c_char;
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let cli = Cli::parse();

    let t0 = Instant::now();
    let schemas = load_driver_schemas_from_ffi().context("load driver schemas from ffi")?;
    let driver_type = unsafe { cstr_to_string(ng_driver_type()) };
    let t_schemas = t0.elapsed();

    tracing::info!(
        driver_type = driver_type,
        locale = cli.locale,
        out_dir = %cli.out_dir.display(),
        schemas_ms = t_schemas.as_millis(),
        "driver schemas loaded"
    );

    let entity = FlattenEntity::DevicePoints;
    let template = schemas.build_template(entity, &cli.locale);

    // Resolve scenarios.
    let scenario_ids = if cli.all {
        (1u8..=7u8).collect::<Vec<_>>()
    } else if cli.scenarios.is_empty() {
        vec![0u8]
    } else {
        cli.scenarios.clone()
    };

    for scenario_id in scenario_ids {
        let preset = if scenario_id == 0 {
            None
        } else {
            Some(ScenarioPreset::get(scenario_id).with_context(|| {
                format!("unknown scenario id: {scenario_id} (supported: 1..=7)")
            })?)
        };

        let channels = cli
            .channels
            .or(preset.as_ref().map(|p| p.channels))
            .context("missing channels (use --scenarios or --channels)")?;
        let devices_per_channel = cli
            .devices_per_channel
            .or(preset.as_ref().map(|p| p.devices_per_channel))
            .context("missing devices_per_channel (use --scenarios or --devices-per-channel)")?;
        let points_per_device = cli
            .points_per_device
            .or(preset.as_ref().map(|p| p.points_per_device))
            .context("missing points_per_device (use --scenarios or --points-per-device)")?;

        tracing::info!(
            scenario = scenario_id,
            channels = channels,
            devices_per_channel = devices_per_channel,
            points_per_device = points_per_device,
            "generating point tables"
        );

        for ch in 1..=channels {
            let out_path = build_output_path(&cli.out_dir, &driver_type, ch, scenario_id);
            write_device_points_workbook(&WriteDevicePointsParams {
                path: &out_path,
                locale: &cli.locale,
                driver_type: &driver_type,
                template: &template,
                channel_index_1based: ch,
                devices_per_channel,
                points_per_device,
                modbus_slave_id_max: cli.modbus_slave_id_max,
                modbus_base_address: cli.modbus_base_address,
                modbus_address_stride: cli.modbus_address_stride,
                opcua_nodeid_template: cli.opcua_nodeid_template.as_str(),
                opcua_nodeid_ns: cli.opcua_nodeid_ns,
                opcua_nodeid_start: cli.opcua_nodeid_start,
            })
            .with_context(|| format!("write workbook {}", out_path.display()))?;
        }
    }

    Ok(())
}

/// Build output file path following the naming convention required by the benchmark docs.
fn build_output_path(
    out_dir: &Path,
    driver_type: &str,
    channel_index_1based: u32,
    scenario_id: u8,
) -> PathBuf {
    let filename = if scenario_id == 0 {
        // Custom scenario (when not using `--all/--scenarios` presets).
        // Keep it numeric to match `{*}` wildcard expectations.
        format!("{driver_type}-scenario0-channel{channel_index_1based}-device-points.xlsx")
    } else {
        format!(
            "{driver_type}-scenario{scenario_id}-channel{channel_index_1based}-device-points.xlsx"
        )
    };
    out_dir.join(filename)
}

/// Parameters for writing a single device-points workbook (one channel).
struct WriteDevicePointsParams<'a> {
    path: &'a Path,
    locale: &'a str,
    driver_type: &'a str,
    template: &'a ng_gateway_sdk::DriverEntityTemplate,
    channel_index_1based: u32,
    devices_per_channel: u32,
    points_per_device: u32,
    modbus_slave_id_max: u32,
    modbus_base_address: u32,
    modbus_address_stride: u32,
    opcua_nodeid_template: &'a str,
    opcua_nodeid_ns: u16,
    opcua_nodeid_start: u32,
}

/// Write a single device+points workbook (one channel worth of rows).
fn write_device_points_workbook(params: &WriteDevicePointsParams<'_>) -> anyhow::Result<()> {
    if let Some(parent) = params.path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create output dir {}", parent.display()))?;
    }

    // Precompute indices of columns we must fill.
    let mut key_to_col: HashMap<&str, usize> =
        HashMap::with_capacity(params.template.columns.len());
    for (i, c) in params.template.columns.iter().enumerate() {
        key_to_col.insert(c.key.as_str(), i);
    }

    let idx_device_name = must_col(&key_to_col, "device_name")?;
    let idx_device_type = must_col(&key_to_col, "device_type")?;
    let idx_point_name = must_col(&key_to_col, "name")?;
    let idx_point_key = must_col(&key_to_col, "key")?;
    let idx_point_type = must_col(&key_to_col, "type")?;
    let idx_point_data_type = must_col(&key_to_col, "data_type")?;
    let idx_point_access_mode = must_col(&key_to_col, "access_mode")?;

    // Optional driver-specific indices
    let idx_modbus_slave_id = key_to_col.get("device_driver_config.slaveId").copied();
    let idx_modbus_function_code = key_to_col.get("driver_config.functionCode").copied();
    let idx_modbus_address = key_to_col.get("driver_config.address").copied();
    let idx_modbus_quantity = key_to_col.get("driver_config.quantity").copied();
    let idx_opcua_node_id = key_to_col.get("driver_config.nodeId").copied();

    // Base enum values (write localized labels, not numeric keys).
    let point_type_label = enum_label_by_key(params.template, "type", params.locale, Json::from(1))
        .context("resolve enum label for point type (Telemetry)")?;
    let data_type_label =
        enum_label_by_key(params.template, "data_type", params.locale, Json::from(9))
            .context("resolve enum label for point data_type (Float32)")?;
    let access_mode_label =
        enum_label_by_key(params.template, "access_mode", params.locale, Json::from(0))
            .context("resolve enum label for point access_mode (Read)")?;

    // Modbus enum label (ReadHoldingRegisters = 3) if present.
    let modbus_function_code_label = if idx_modbus_function_code.is_some() {
        enum_label_by_key(
            params.template,
            "driver_config.functionCode",
            params.locale,
            Json::from(3),
        )
        .context("resolve enum label for modbus functionCode (ReadHoldingRegisters)")?
    } else {
        String::new()
    };

    // Worksheet / workbook
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    let _ = worksheet.set_name("device-points");

    // Header style (match gateway template visual cues loosely; not functionally required).
    let header_format = Format::new()
        .set_bold()
        .set_font_size(12.0)
        .set_background_color(Color::RGB(0xE6F7FF))
        .set_pattern(FormatPattern::Solid);

    for (ci, col) in params.template.columns.iter().enumerate() {
        worksheet
            .write_string_with_format(0, ci as u16, &col.label, &header_format)
            .with_context(|| format!("write header col={}", ci))?;
    }

    // Generate rows.
    // One row per point, repeating device columns for each point row.
    let mut row_idx: u32 = 1;
    let devices_per_channel = params.devices_per_channel;
    let channel_index_1based = params.channel_index_1based;
    for device_i in 1..=devices_per_channel {
        let device_name = format!("ch{channel_index_1based}-dev{device_i}");
        let device_type_value = default_device_type(params.driver_type);

        // Modbus slave id assignment:
        // - Use a global device index across channels so that e.g. 10 channels × 1 device
        //   becomes slaveId 1..=10 instead of all 1.
        // - Wrap within `modbus_slave_id_max` (default 10).
        // - Always clamp to Modbus driver's validation range [1, 247].
        let slave_id_max = params.modbus_slave_id_max.clamp(1, 247);
        let global_device_index_1based =
            (channel_index_1based.saturating_sub(1)).saturating_mul(devices_per_channel) + device_i;
        let modbus_slave_id_value =
            ((global_device_index_1based.saturating_sub(1)) % slave_id_max) + 1;

        for point_i in 1..=params.points_per_device {
            // Base fields
            worksheet.write_string(row_idx, idx_device_name as u16, &device_name)?;
            worksheet.write_string(row_idx, idx_device_type as u16, device_type_value)?;

            // Device driver config (Modbus only)
            if let Some(col) = idx_modbus_slave_id {
                worksheet.write_number(row_idx, col as u16, modbus_slave_id_value as f64)?;
            }

            // Point base
            let point_name = format!("p{point_i}");
            let point_key = format!("p{point_i}");
            worksheet.write_string(row_idx, idx_point_name as u16, &point_name)?;
            worksheet.write_string(row_idx, idx_point_key as u16, &point_key)?;
            worksheet.write_string(row_idx, idx_point_type as u16, &point_type_label)?;
            worksheet.write_string(row_idx, idx_point_data_type as u16, &data_type_label)?;
            worksheet.write_string(row_idx, idx_point_access_mode as u16, &access_mode_label)?;

            // Modbus point driver config
            if let (Some(fc), Some(addr), Some(qty)) = (
                idx_modbus_function_code,
                idx_modbus_address,
                idx_modbus_quantity,
            ) {
                // Address layout: sequential by point index, per device uses same address range.
                // For Float32 we default quantity=2 and stride=2.
                let base = params.modbus_base_address;
                let stride = params.modbus_address_stride.max(1);
                let address = base.saturating_add((point_i - 1).saturating_mul(stride));

                worksheet.write_string(row_idx, fc as u16, &modbus_function_code_label)?;
                worksheet.write_number(row_idx, addr as u16, address as f64)?;
                // Float32 -> 2 registers (typical). Keep configurable by stride; default qty == stride.
                worksheet.write_number(row_idx, qty as u16, stride as f64)?;
            }

            // OPC UA point driver config
            if let Some(node_id_col) = idx_opcua_node_id {
                let node_id = render_node_id(
                    params.opcua_nodeid_template,
                    params.opcua_nodeid_ns,
                    params.opcua_nodeid_start,
                    channel_index_1based,
                    device_i,
                    point_i,
                );
                worksheet.write_string(row_idx, node_id_col as u16, &node_id)?;
            }

            row_idx += 1;
        }
    }

    // Hidden __meta__ sheet required by import validation.
    append_meta_sheet(
        &mut workbook,
        &TemplateMetadata {
            driver_type: params.driver_type.to_string(),
            driver_version: None,
            api_version: None,
            entity: FlattenEntity::DevicePoints.to_string().to_ascii_lowercase(),
            locale: params.locale.to_string(),
            schema_version: "1.0".to_string(),
        },
    )?;

    workbook
        .save(params.path)
        .with_context(|| format!("save workbook {}", params.path.display()))?;

    tracing::info!(
        file = %params.path.display(),
        rows = row_idx.saturating_sub(1),
        "point table generated"
    );

    Ok(())
}

/// Append the hidden `__meta__` worksheet.
fn append_meta_sheet(workbook: &mut Workbook, metadata: &TemplateMetadata) -> anyhow::Result<()> {
    let meta_ws = workbook.add_worksheet();
    let _ = meta_ws.set_hidden(true).set_name("__meta__");

    meta_ws.write_string(0u32, 0u16, "key")?;
    meta_ws.write_string(0u32, 1u16, "value")?;

    let items: [(&str, String); 6] = [
        ("driver_type", metadata.driver_type.clone()),
        (
            "driver_version",
            metadata.driver_version.clone().unwrap_or_default(),
        ),
        (
            "api_version",
            metadata.api_version.clone().unwrap_or_default(),
        ),
        ("entity", metadata.entity.clone()),
        ("locale", metadata.locale.clone()),
        ("schema_version", metadata.schema_version.clone()),
    ];

    for (idx, (k, v)) in items.iter().enumerate() {
        let r = (idx as u32) + 1;
        meta_ws.write_string(r, 0u16, *k)?;
        meta_ws.write_string(r, 1u16, v)?;
    }

    Ok(())
}

/// Resolve an enum label by matching the enum key in template columns.
fn enum_label_by_key(
    template: &ng_gateway_sdk::DriverEntityTemplate,
    key: &str,
    locale: &str,
    enum_key: Json,
) -> Option<String> {
    let col = template.columns.iter().find(|c| c.key == key)?;
    match &col.data_type {
        UiDataType::Enum { items } => {
            let localized = UiDataType::localize_enum_items(items.as_slice(), locale);
            localized
                .into_iter()
                .find(|(k, _)| *k == enum_key)
                .map(|(_, label)| label)
        }
        _ => None,
    }
}

/// Choose a reasonable default device_type for benchmark templates.
fn default_device_type(driver_type: &str) -> &'static str {
    match driver_type {
        "modbus" => "modbus-tcp-slave",
        "opcua" => "opcua-device",
        _ => "device",
    }
}

/// Simple placeholder renderer for OPC UA NodeId template strings.
fn render_node_id(
    template: &str,
    ns: u16,
    start_i: u32,
    channel: u32,
    device: u32,
    point: u32,
) -> String {
    // `node_i` is a convenient numeric node id for common simulators (e.g. Prosys):
    // ns={ns};i={start..start+count-1}
    // Keep this deterministic and 1-based.
    let node_i = start_i.saturating_add(point.saturating_sub(1));
    template
        .replace("{channel}", &channel.to_string())
        .replace("{device}", &device.to_string())
        .replace("{point}", &point.to_string())
        .replace("{ns}", &ns.to_string())
        .replace("{node_i}", &node_i.to_string())
}

fn must_col(map: &HashMap<&str, usize>, key: &str) -> anyhow::Result<usize> {
    map.get(key)
        .copied()
        .with_context(|| format!("missing required column key: {key}"))
}

/// Load `DriverSchemas` from the driver-exported metadata JSON bytes.
fn load_driver_schemas_from_ffi() -> anyhow::Result<DriverSchemas> {
    let mut ptr: *const u8 = ptr::null();
    let mut len: usize = 0;
    unsafe { ng_driver_metadata_json_ptr(&mut ptr as *mut *const u8, &mut len as *mut usize) };
    if ptr.is_null() || len == 0 {
        anyhow::bail!("driver returned empty metadata json");
    }
    let bytes = unsafe { slice::from_raw_parts(ptr, len) };
    Ok(serde_json::from_slice::<DriverSchemas>(bytes)?)
}

/// Convert a C string pointer into a Rust `String`.
fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr).to_string_lossy().to_string() }
}
