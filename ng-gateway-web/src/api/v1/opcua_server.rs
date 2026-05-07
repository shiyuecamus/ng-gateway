//! OPC UA Server northward app HTTP endpoints.
//!
//! # Layering
//! This module is intentionally **OPC UA protocol-agnostic** beyond the
//! capability id and the inspector DTO it consumes. All wire-format
//! derivations (NodeId, BrowsePath, AccessLevel, type names, discovery URLs)
//! are produced by the `opcua-server` plugin and reach this module pre-rendered
//! through `OpcuaServerRuntimeSnapshot::materialized`. The web layer is a
//! pure renderer — its only job is to:
//!
//! 1. Authorize the request via RBAC.
//! 2. Resolve `AppInfo` + `PluginInfo` for overview metadata.
//! 3. Invoke `NorthwardManager::invoke_app_capability(...)` (no
//!    `downcast_arc` — the trait itself surfaces capability invocation).
//! 4. Render the snapshot into a deterministic XLSX workbook in a blocking
//!    pool to avoid stalling actix workers.

use super::app::ROUTER_PREFIX as APP_ROUTER_PREFIX;
use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};
use actix_web::{
    http::{
        header::{
            self, Charset, ContentDisposition, DispositionParam, DispositionType, ExtendedValue,
        },
        Method, StatusCode,
    },
    web, HttpResponse,
};
use actix_web_validator::Path;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{OpcuaServerExportContext, PathId},
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    Gateway, PermChecker,
};
use ng_gateway_repository::{AppRepository, PluginRepository};
use ng_gateway_sdk::northward::opcua_server::{
    InspectorRequestV1, InspectorResponseV1, MaterializedNode, OpcuaServerRuntimeSnapshot,
    CAPABILITY_INSPECTOR_V1,
};
use rust_xlsxwriter::{Format, Workbook, Worksheet, XlsxError};
use std::sync::Arc;
use tracing::{info, instrument};

/// Plugin type identifier this endpoint accepts.
const OPCUA_SERVER_PLUGIN_TYPE: &str = "opcua-server";

/// XLSX MIME for OOXML spreadsheets (RFC 4839 / OOXML §3).
const XLSX_CONTENT_TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// Stable header layout for the Points sheet.
///
/// Layout & ordering are part of the public export contract; do not reorder
/// without bumping the export schema version (separate from `inspector:vN`).
const POINT_HEADERS: [&str; 20] = [
    "#",
    "Channel Name",
    "Device Name",
    "Point Key",
    "Point Name",
    "Description",
    "Type",
    "NodeId",
    "Browse Path",
    "Wire DataType",
    "OPC UA DataType",
    "Logical DataType",
    "AccessMode",
    "OPC UA AccessLevel",
    "Unit",
    "Min Value",
    "Max Value",
    "Transform Scale",
    "Transform Offset",
    "Transform Negate",
];

/// Stable label rows for the Overview sheet.
///
/// Layout & ordering are part of the public export contract; do not reorder
/// without bumping the export schema version (separate from `inspector:vN`).
const OVERVIEW_FIELDS: [&str; 17] = [
    "App Name",
    "Plugin Type",
    "Plugin Version",
    "Namespace URI",
    "Application URI",
    "Product URI",
    "Bind Address",
    "Advertised Endpoints",
    "Certificate Thumbprint",
    "Certificate CN",
    "Certificate SAN URI",
    "Certificate SAN Hostnames",
    "Certificate SAN IPs",
    "Certificate Not Before",
    "Certificate Not After",
    "Certificate Health",
    "Total Materialized Points",
];

/// Configure OPC UA Server app routes.
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/{id}/opcua-server/export-points.xlsx",
        web::get().to(export_points),
    );
}

/// Initialize RBAC rules for OPC UA Server app export APIs.
#[inline]
#[instrument(name = "init-opcua-server-app-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    let rules = vec![(
        Method::GET,
        format!("{router_prefix}{APP_ROUTER_PREFIX}/{{id}}/opcua-server/export-points.xlsx"),
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
            .or(has_resource_operation(EntityType::App, Operation::Read)?)
            .or(has_scope("northward-app:read")?),
    )];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    info!("OPC UA Server app RBAC rules initialized successfully");
    Ok(())
}

#[instrument(name = "export-opcua-server-points", skip_all, fields(app_id = params.id))]
async fn export_points(
    params: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<HttpResponse> {
    let context = build_export_context(params.id, Arc::clone(&state.gateway)).await?;
    let filename = make_download_filename(&context.app_name);
    let content_disposition = build_content_disposition(filename.clone());

    // Excel generation is CPU-bound and synchronous; offload to a blocking pool
    // so we don't stall actix workers on large exports.
    let owned_context = context;
    let bytes = web::block(move || render_workbook(&owned_context).map_err(|e| e.to_string()))
        .await
        .map_err(|e| WebError::InternalError(format!("Excel generation task failed: {e}")))?
        .map_err(|e| WebError::InternalError(format!("Excel generation failed: {e}")))?;

    Ok(HttpResponse::build(StatusCode::OK)
        .insert_header((header::CONTENT_TYPE, XLSX_CONTENT_TYPE))
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .insert_header((
            header::HeaderName::from_static("x-content-type-options"),
            "nosniff",
        ))
        .insert_header(content_disposition)
        .body(bytes))
}

/// Resolve all inputs needed to render the export workbook.
async fn build_export_context(
    app_id: i32,
    gateway: Arc<dyn Gateway>,
) -> WebResult<OpcuaServerExportContext> {
    let app = AppRepository::find_info_by_id(app_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::App.to_string()))?;

    if normalize_plugin_type(&app.plugin_type) != OPCUA_SERVER_PLUGIN_TYPE {
        return Err(WebError::BadRequest(format!(
            "plugin type '{}' does not support OPC UA Server point export",
            app.plugin_type
        )));
    }

    let plugin = PluginRepository::find_info_by_id(app.plugin_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Plugin.to_string()))?;

    // Capability invocation is a control-plane operation; the trait surface
    // means we never touch a concrete northward manager type from the web layer.
    let request = serde_json::to_value(InspectorRequestV1::Snapshot)
        .map_err(|e| WebError::InternalError(format!("Failed to encode inspector request: {e}")))?;
    let raw = gateway
        .northward_manager()
        .invoke_app_capability(app_id, CAPABILITY_INSPECTOR_V1, request)
        .await
        .map_err(WebError::from)?;
    let snapshot = decode_snapshot(raw)?;

    Ok(OpcuaServerExportContext {
        app_name: app.name,
        plugin_type: plugin.plugin_type,
        plugin_version: plugin.version,
        snapshot,
    })
}

fn decode_snapshot(response: serde_json::Value) -> WebResult<OpcuaServerRuntimeSnapshot> {
    let response: InspectorResponseV1 = serde_json::from_value(response).map_err(|e| {
        WebError::InternalError(format!(
            "Failed to decode OPC UA Server inspector response: {e}"
        ))
    })?;
    match response {
        InspectorResponseV1::Snapshot(snapshot) => Ok(snapshot),
    }
}

#[inline]
fn normalize_plugin_type(plugin_type: &str) -> &str {
    plugin_type.split(':').next().unwrap_or(plugin_type)
}

fn build_content_disposition(filename: String) -> ContentDisposition {
    ContentDisposition {
        disposition: DispositionType::Attachment,
        parameters: vec![
            DispositionParam::Filename(filename.clone()),
            DispositionParam::FilenameExt(ExtendedValue {
                charset: Charset::Ext("UTF-8".to_string()),
                language_tag: None,
                value: filename.into_bytes(),
            }),
        ],
    }
}

/// Build a safe ASCII-only download filename from the app display name.
///
/// Non-ASCII characters are replaced **per scalar value** (not per byte) so
/// CJK app names degrade to single dashes per glyph. The fallback `app` name
/// guarantees we never emit `opcua-server-points-.xlsx`.
fn make_download_filename(app_name: &str) -> String {
    let mut safe = String::with_capacity(app_name.len());
    for ch in app_name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            safe.push(ch);
        } else {
            safe.push('-');
        }
    }
    if safe.is_empty() {
        safe.push_str("app");
    }
    format!("opcua-server-points-{safe}.xlsx")
}

/// Render the export context into XLSX bytes (Sheet 1 Overview + Sheet 2 Points).
fn render_workbook(context: &OpcuaServerExportContext) -> Result<Vec<u8>, XlsxError> {
    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold();
    let node_id_format = Format::new().set_font_name("Consolas").set_bold();

    {
        let worksheet = workbook.add_worksheet();
        worksheet.set_name("Overview")?;
        write_overview_sheet(worksheet, context, &header_format)?;
    }

    {
        let worksheet = workbook.add_worksheet();
        worksheet.set_name("Points")?;
        write_points_sheet(
            worksheet,
            &context.snapshot.materialized,
            &header_format,
            &node_id_format,
        )?;
    }

    workbook.save_to_buffer()
}

fn write_overview_sheet(
    worksheet: &mut Worksheet,
    context: &OpcuaServerExportContext,
    header_format: &Format,
) -> Result<(), XlsxError> {
    worksheet.write_with_format(0, 0, "Field", header_format)?;
    worksheet.write_with_format(0, 1, "Value", header_format)?;

    let snapshot = &context.snapshot;
    let advertised_endpoints = snapshot.advertised_endpoints.join("\n");
    let total_points = snapshot.materialized.len();

    // String-valued rows (positions 1..=8) — Field 9..=15 may also be strings
    // when the certificate summary is present, otherwise we render a single
    // "PKI not ready" placeholder.
    let mut row: u32 = 1;
    let write_kv = |worksheet: &mut Worksheet,
                    label_idx: usize,
                    value: &str,
                    row_ref: &mut u32|
     -> Result<(), XlsxError> {
        worksheet.write(*row_ref, 0, OVERVIEW_FIELDS[label_idx])?;
        worksheet.write(*row_ref, 1, value)?;
        *row_ref += 1;
        Ok(())
    };

    write_kv(worksheet, 0, context.app_name.as_str(), &mut row)?;
    write_kv(worksheet, 1, context.plugin_type.as_str(), &mut row)?;
    write_kv(worksheet, 2, context.plugin_version.as_str(), &mut row)?;
    write_kv(worksheet, 3, snapshot.namespace_uri.as_str(), &mut row)?;
    write_kv(worksheet, 4, snapshot.application_uri.as_str(), &mut row)?;
    write_kv(worksheet, 5, snapshot.product_uri.as_str(), &mut row)?;
    write_kv(worksheet, 6, snapshot.bind_addr.as_str(), &mut row)?;
    write_kv(worksheet, 7, advertised_endpoints.as_str(), &mut row)?;

    if let Some(cert) = snapshot.cert_summary.as_ref() {
        let san_hosts = cert.san_hostnames.join("\n");
        let san_ips = cert.san_ips.join("\n");
        let not_before = cert.not_before.to_rfc3339();
        let not_after = cert.not_after.to_rfc3339();
        let health = format!("{} ({} days to expiry)", cert.health, cert.days_to_expiry);
        write_kv(worksheet, 8, cert.thumbprint_hex.as_str(), &mut row)?;
        write_kv(worksheet, 9, cert.common_name.as_str(), &mut row)?;
        write_kv(worksheet, 10, cert.san_uri.as_str(), &mut row)?;
        write_kv(worksheet, 11, san_hosts.as_str(), &mut row)?;
        write_kv(worksheet, 12, san_ips.as_str(), &mut row)?;
        write_kv(worksheet, 13, not_before.as_str(), &mut row)?;
        write_kv(worksheet, 14, not_after.as_str(), &mut row)?;
        write_kv(worksheet, 15, health.as_str(), &mut row)?;
    } else {
        const PLACEHOLDER: &str = "(PKI not ready)";
        for idx in 8..=15 {
            write_kv(worksheet, idx, PLACEHOLDER, &mut row)?;
        }
    }

    // The total-points row is the only one carrying a numeric value, so we
    // write it without going through the string helper.
    worksheet.write(row, 0, OVERVIEW_FIELDS[16])?;
    worksheet.write(row, 1, total_points as u32)?;

    worksheet.set_freeze_panes(1, 0)?;
    worksheet.autofit();
    Ok(())
}

fn write_points_sheet(
    worksheet: &mut Worksheet,
    rows: &[MaterializedNode],
    header_format: &Format,
    node_id_format: &Format,
) -> Result<(), XlsxError> {
    for (col, header) in POINT_HEADERS.iter().enumerate() {
        worksheet.write_with_format(0, col as u16, *header, header_format)?;
    }

    for (idx, row) in rows.iter().enumerate() {
        let excel_row = (idx + 1) as u32;
        let seq = (idx + 1) as u32;
        worksheet.write(excel_row, 0, seq)?;
        worksheet.write(excel_row, 1, row.channel_name.as_str())?;
        worksheet.write(excel_row, 2, row.device_name.as_str())?;
        worksheet.write(excel_row, 3, row.point_key.as_str())?;
        worksheet.write(excel_row, 4, row.point_name.as_str())?;
        if let Some(description) = row.description.as_ref() {
            worksheet.write(excel_row, 5, description.as_str())?;
        }
        worksheet.write(excel_row, 6, row.point_type.as_str())?;
        worksheet.write_with_format(excel_row, 7, row.node_id.as_str(), node_id_format)?;
        worksheet.write(excel_row, 8, row.browse_path.as_str())?;
        worksheet.write(excel_row, 9, row.wire_data_type.as_str())?;
        worksheet.write(excel_row, 10, row.opcua_data_type.as_str())?;
        worksheet.write(excel_row, 11, row.logical_data_type.as_str())?;
        worksheet.write(excel_row, 12, row.access_mode.as_str())?;
        worksheet.write(excel_row, 13, row.opcua_access_level.as_str())?;
        if let Some(unit) = row.unit.as_ref() {
            worksheet.write(excel_row, 14, unit.as_str())?;
        }
        if let Some(min_value) = row.min_value {
            worksheet.write(excel_row, 15, min_value)?;
        }
        if let Some(max_value) = row.max_value {
            worksheet.write(excel_row, 16, max_value)?;
        }
        if let Some(scale) = row.transform_scale {
            worksheet.write(excel_row, 17, scale)?;
        }
        if let Some(offset) = row.transform_offset {
            worksheet.write(excel_row, 18, offset)?;
        }
        worksheet.write(excel_row, 19, row.transform_negate)?;
    }

    let last_row = rows.len() as u32;
    worksheet.autofilter(0, 0, last_row, (POINT_HEADERS.len() - 1) as u16)?;
    worksheet.set_freeze_panes(1, 0)?;
    worksheet.autofit();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
    use std::io::Cursor;

    fn make_context(materialized: Vec<MaterializedNode>) -> OpcuaServerExportContext {
        OpcuaServerExportContext {
            app_name: "demo".to_string(),
            plugin_type: "opcua-server".to_string(),
            plugin_version: "0.1.0".to_string(),
            snapshot: OpcuaServerRuntimeSnapshot {
                namespace_index: 1,
                namespace_uri: "urn:test:namespace".to_string(),
                application_uri: "urn:test:app".to_string(),
                product_uri: "urn:test:product".to_string(),
                bind_addr: "0.0.0.0:4840".to_string(),
                advertised_endpoints: vec!["opc.tcp://gateway.local:4840/".to_string()],
                cert_summary: None,
                materialized,
            },
        }
    }

    fn sample_node(seq: u32) -> MaterializedNode {
        MaterializedNode {
            point_id: seq as i32,
            channel_name: "channel-a".to_string(),
            device_name: "device-1".to_string(),
            point_key: format!("temp{seq}"),
            point_name: format!("Temperature {seq}"),
            description: Some("ambient".to_string()),
            point_type: "telemetry".to_string(),
            access_mode: "read".to_string(),
            wire_data_type: "float32".to_string(),
            logical_data_type: "float32".to_string(),
            node_id: format!("ns=1;s=channel-a/device-1/temp{seq}"),
            browse_path: format!("/Objects/NG-Gateway/channel-a/device-1/temp{seq}"),
            opcua_data_type: "Float".to_string(),
            opcua_access_level: "CurrentRead".to_string(),
            unit: Some("℃".to_string()),
            min_value: Some(-40.0),
            max_value: Some(120.0),
            transform_scale: Some(1.0),
            transform_offset: Some(0.0),
            transform_negate: false,
        }
    }

    fn open(bytes: Vec<u8>) -> Xlsx<Cursor<Vec<u8>>> {
        open_workbook_from_rs(Cursor::new(bytes)).expect("workbook should be parseable")
    }

    #[test]
    fn emits_overview_and_points_sheets() {
        let ctx = make_context(vec![sample_node(1), sample_node(2)]);
        let bytes = render_workbook(&ctx).expect("workbook render must succeed");
        let workbook = open(bytes);
        assert_eq!(
            workbook.sheet_names(),
            vec!["Overview".to_string(), "Points".to_string()]
        );
    }

    #[test]
    fn overview_sheet_emits_full_canonical_field_layout() {
        let ctx = make_context(vec![sample_node(1)]);
        let bytes = render_workbook(&ctx).unwrap();
        let mut wb = open(bytes);
        let range = wb
            .worksheet_range("Overview")
            .expect("Overview sheet must exist");

        assert_eq!(range.get_value((0, 0)), Some(&Data::String("Field".into())));
        assert_eq!(range.get_value((0, 1)), Some(&Data::String("Value".into())));
        for (offset, field) in OVERVIEW_FIELDS.iter().enumerate() {
            let row = (offset + 1) as u32;
            assert_eq!(
                range.get_value((row, 0)),
                Some(&Data::String((*field).to_string())),
                "field row {row} mismatch (field = {field})"
            );
        }
    }

    #[test]
    fn overview_sheet_renders_pki_placeholder_when_summary_absent() {
        let ctx = make_context(vec![sample_node(1)]);
        let bytes = render_workbook(&ctx).unwrap();
        let mut wb = open(bytes);
        let range = wb.worksheet_range("Overview").unwrap();

        // Bind Address / Advertised Endpoints follow the static identity rows.
        assert_eq!(
            range.get_value((7, 1)),
            Some(&Data::String("0.0.0.0:4840".to_string()))
        );
        assert!(range
            .get_value((8, 1))
            .map(|v| matches!(v, Data::String(s) if s.contains("opc.tcp://gateway.local:4840/")))
            .unwrap_or(false));

        // The 8 cert rows must all carry the placeholder when summary is None.
        for row in 9..=16 {
            assert_eq!(
                range.get_value((row, 1)),
                Some(&Data::String("(PKI not ready)".to_string())),
                "row {row} must carry placeholder"
            );
        }
    }

    #[test]
    fn overview_sheet_renders_cert_summary_when_present() {
        use chrono::TimeZone;
        let mut ctx = make_context(vec![sample_node(1)]);
        ctx.snapshot.cert_summary = Some(
            ng_gateway_sdk::northward::opcua_server::OpcuaServerCertSummary {
                thumbprint_hex: "abcdef0123".to_string(),
                common_name: "NG-Gateway OPC UA Server".to_string(),
                san_uri: "urn:ng:opcua-server".to_string(),
                san_hostnames: vec!["gateway.local".to_string()],
                san_ips: vec!["192.168.1.10".to_string()],
                not_before: chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                not_after: chrono::Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(),
                days_to_expiry: 240,
                health: "healthy".to_string(),
            },
        );
        let bytes = render_workbook(&ctx).unwrap();
        let mut wb = open(bytes);
        let range = wb.worksheet_range("Overview").unwrap();

        // thumbprint row (offset 9)
        assert_eq!(
            range.get_value((9, 1)),
            Some(&Data::String("abcdef0123".to_string()))
        );
        // health row (offset 16)
        assert!(range
            .get_value((16, 1))
            .map(|v| matches!(v, Data::String(s) if s.contains("healthy")))
            .unwrap_or(false));
    }

    #[test]
    fn points_sheet_uses_stable_header_layout() {
        let ctx = make_context(vec![sample_node(1)]);
        let bytes = render_workbook(&ctx).unwrap();
        let mut wb = open(bytes);
        let range = wb.worksheet_range("Points").unwrap();
        for (col, header) in POINT_HEADERS.iter().enumerate() {
            assert_eq!(
                range.get_value((0, col as u32)),
                Some(&Data::String((*header).to_string())),
                "header column {col} mismatch"
            );
        }
        assert_eq!(
            range.get_value((1, 7)),
            Some(&Data::String("ns=1;s=channel-a/device-1/temp1".to_string())),
        );
    }

    #[test]
    fn empty_materialized_yields_workbook_with_only_headers() {
        let ctx = make_context(Vec::new());
        let bytes = render_workbook(&ctx).unwrap();
        let mut wb = open(bytes);
        let range = wb.worksheet_range("Points").unwrap();
        assert_eq!(range.height(), 1);
        assert_eq!(range.width(), POINT_HEADERS.len());
    }

    #[test]
    fn make_download_filename_handles_unsafe_chars() {
        assert_eq!(
            make_download_filename("hello"),
            "opcua-server-points-hello.xlsx"
        );
        assert_eq!(
            make_download_filename("App / Demo"),
            "opcua-server-points-App---Demo.xlsx"
        );
        assert_eq!(make_download_filename(""), "opcua-server-points-app.xlsx");
        assert_eq!(
            make_download_filename("北向应"),
            "opcua-server-points----.xlsx",
            "non-ASCII collapses per scalar value"
        );
    }

    #[test]
    fn normalize_plugin_type_drops_namespace_qualifier() {
        assert_eq!(normalize_plugin_type("opcua-server"), "opcua-server");
        assert_eq!(normalize_plugin_type("opcua-server:v2"), "opcua-server");
    }
}
