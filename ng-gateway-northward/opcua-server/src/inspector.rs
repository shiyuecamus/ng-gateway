//! OPC UA Server runtime capability inspector.
//!
//! # Role
//! The inspector is the connector-owned implementation of the `inspector:v1`
//! capability defined in `ng_gateway_sdk::northward::opcua_server`. It stitches
//! together three orthogonal sources to produce a self-contained, host-readable
//! snapshot:
//!
//! - `OpcuaServerPluginConfig` — static identity (URIs, bind/advertised hosts)
//! - `RuntimeSubscriber` — live runtime metadata published by the active
//!   session (namespace index, validated advertised endpoints, certificate
//!   summary)
//! - `NodeCache` + `NorthwardRuntimeApi` — per-point bindings + gateway
//!   metadata (`PointMeta`)
//!
//! All OPC UA-specific derivations (NodeId, BrowsePath, type/access labels)
//! flow through `crate::protocol`, keeping this module purely an aggregator.
//!
//! # Latency
//! Snapshot construction is synchronous and bounded by `O(materialized_count)`
//! of `dashmap` reads. There is no IO or locking on the hot path; the host can
//! call this freely from control-plane handlers.

use crate::{
    config::OpcuaServerPluginConfig,
    node_cache::NodeCache,
    pki::CertSummary,
    protocol::{
        access_mode_label, data_type_label, opcua_access_level_label, opcua_data_type_name,
        point_type_label,
    },
    publication::{RuntimePublication, RuntimeSubscriber},
};
use ng_gateway_sdk::{
    northward::opcua_server::{
        InspectorRequestV1, InspectorResponseV1, MaterializedNode, OpcuaServerCertSummary,
        OpcuaServerRuntimeSnapshot,
    },
    NorthwardError, NorthwardResult, NorthwardRuntimeApi,
};
use std::sync::Arc;
use tracing::warn;

/// Pre-publication fallback values for inspector snapshots.
///
/// Returned when no session has yet published runtime metadata. The values
/// are derived purely from `OpcuaServerPluginConfig` and document the fact
/// that the snapshot reflects "configuration intent" rather than "running
/// state".
struct FallbackPublication {
    namespace_index: u16,
    bind_addr: String,
    advertised_endpoints: Vec<String>,
}

impl FallbackPublication {
    /// Fall back to namespace index `1` (the order in which the gateway
    /// namespace is registered) and copy advertised endpoints verbatim. Note
    /// that pre-validation defects (empty list, wildcard host, etc.) surface
    /// in the snapshot exactly as configured so the operator sees what is
    /// actually persisted.
    fn from_config(config: &OpcuaServerPluginConfig) -> Self {
        Self {
            namespace_index: 1,
            bind_addr: config.bind_addr.clone(),
            advertised_endpoints: config.advertised_endpoints.clone(),
        }
    }
}

/// Connector-owned inspector for the `inspector:v1` capability.
///
/// Holds only `Arc` clones; safe to share across tasks.
pub struct OpcuaServerInspector {
    config: Arc<OpcuaServerPluginConfig>,
    runtime_api: Arc<dyn NorthwardRuntimeApi>,
    node_cache: Arc<NodeCache>,
    runtime_rx: RuntimeSubscriber,
}

impl OpcuaServerInspector {
    /// Create an inspector wired to the connector's shared dependencies.
    pub fn new(
        config: Arc<OpcuaServerPluginConfig>,
        runtime_api: Arc<dyn NorthwardRuntimeApi>,
        node_cache: Arc<NodeCache>,
        runtime_rx: RuntimeSubscriber,
    ) -> Self {
        Self {
            config,
            runtime_api,
            node_cache,
            runtime_rx,
        }
    }

    /// Decode the typed request, dispatch the handler, and re-encode the response.
    pub async fn handle(&self, request: serde_json::Value) -> NorthwardResult<serde_json::Value> {
        let req: InspectorRequestV1 =
            serde_json::from_value(request).map_err(|e| NorthwardError::ValidationFailed {
                reason: format!("invalid OPC UA Server inspector request: {e}"),
            })?;

        match req {
            InspectorRequestV1::Snapshot => {
                let response = InspectorResponseV1::Snapshot(self.snapshot());
                serde_json::to_value(response).map_err(Into::into)
            }
        }
    }

    /// Build a self-contained snapshot. All OPC UA-specific derivations happen
    /// here so the host never reproduces protocol logic.
    fn snapshot(&self) -> OpcuaServerRuntimeSnapshot {
        let (namespace_index, bind_addr, advertised_endpoints, cert_summary) =
            self.publication_or_fallback();
        let materialized = self.materialized_view();

        OpcuaServerRuntimeSnapshot {
            namespace_index,
            namespace_uri: self.config.namespace_uri.clone(),
            application_uri: self.config.application_uri.clone(),
            product_uri: self.config.product_uri.clone(),
            bind_addr,
            advertised_endpoints,
            cert_summary: cert_summary.map(cert_summary_to_dto),
            materialized,
        }
    }

    /// Return the live publication fields, or a configuration-driven fallback.
    fn publication_or_fallback(&self) -> (u16, String, Vec<String>, Option<CertSummary>) {
        match self.runtime_rx.borrow().clone() {
            Some(RuntimePublication {
                namespace_index,
                bind_addr,
                advertised_endpoints,
                cert_summary,
            }) => (
                namespace_index,
                bind_addr,
                advertised_endpoints,
                cert_summary,
            ),
            None => {
                let fallback = FallbackPublication::from_config(self.config.as_ref());
                (
                    fallback.namespace_index,
                    fallback.bind_addr,
                    fallback.advertised_endpoints,
                    None,
                )
            }
        }
    }

    /// Walk the NodeCache snapshot and assemble self-contained materialized rows.
    ///
    /// Stale `point_id` entries (e.g. point removed concurrently) are skipped
    /// with a warning so the export stays consistent without surfacing
    /// transient races to the caller.
    fn materialized_view(&self) -> Vec<MaterializedNode> {
        let bindings = self.node_cache.snapshot_materialized();
        let mut out = Vec::with_capacity(bindings.len());

        for (point_id, node_id) in bindings {
            let Some(meta) = self.runtime_api.get_point_meta(point_id) else {
                warn!(
                    point_id,
                    "skipping materialized OPC UA NodeId; PointMeta no longer present in runtime index"
                );
                continue;
            };

            let logical_data_type = meta.logical_data_type();
            let wire_data_type = meta.wire_data_type();

            out.push(MaterializedNode {
                point_id,
                channel_name: meta.channel_name.as_ref().to_string(),
                device_name: meta.device_name.as_ref().to_string(),
                point_key: meta.point_key.as_ref().to_string(),
                point_name: meta.point_name.as_ref().to_string(),
                description: meta.description.as_ref().map(|s| s.as_ref().to_string()),
                point_type: point_type_label(meta.point_type).to_string(),
                access_mode: access_mode_label(meta.access_mode).to_string(),
                wire_data_type: data_type_label(wire_data_type).to_string(),
                logical_data_type: data_type_label(logical_data_type).to_string(),
                node_id: node_id.to_string(),
                browse_path: crate::protocol::make_browse_path(
                    meta.channel_name.as_ref(),
                    meta.device_name.as_ref(),
                    meta.point_key.as_ref(),
                ),
                opcua_data_type: opcua_data_type_name(wire_data_type).to_string(),
                opcua_access_level: opcua_access_level_label(meta.access_mode).to_string(),
                unit: meta.unit.as_ref().map(|s| s.as_ref().to_string()),
                min_value: meta.min_value,
                max_value: meta.max_value,
                transform_scale: meta.transform.transform_scale,
                transform_offset: meta.transform.transform_offset,
                transform_negate: meta.transform.transform_negate,
            });
        }

        // Stable presentation order for export determinism: by gateway path.
        out.sort_by(|a, b| {
            a.channel_name
                .cmp(&b.channel_name)
                .then_with(|| a.device_name.cmp(&b.device_name))
                .then_with(|| a.point_key.cmp(&b.point_key))
        });
        out
    }
}

fn cert_summary_to_dto(s: CertSummary) -> OpcuaServerCertSummary {
    OpcuaServerCertSummary {
        thumbprint_hex: s.thumbprint_hex,
        common_name: s.common_name,
        san_uri: s.san_uri,
        san_hostnames: s.san_hostnames,
        san_ips: s.san_ips,
        not_before: s.not_before,
        not_after: s.not_after,
        days_to_expiry: s.days_to_expiry,
        health: s.health.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publication::{channel, RuntimePublicationGuard};
    use async_trait::async_trait;
    use ng_gateway_sdk::{
        northward::opcua_server::CAPABILITY_INSPECTOR_V1, AccessMode, DataPointType, DataType,
        PointMeta, RuntimeDelta, Transform,
    };
    use opcua::types::NodeId;
    use serde_json::json;
    use tokio::sync::broadcast;

    /// Minimal NorthwardRuntimeApi double for inspector tests.
    struct FakeRuntime {
        points: dashmap::DashMap<i32, Arc<PointMeta>>,
    }

    impl FakeRuntime {
        fn new() -> Self {
            Self {
                points: dashmap::DashMap::new(),
            }
        }

        fn with(self, meta: PointMeta) -> Self {
            self.points.insert(meta.point_id, Arc::new(meta));
            self
        }

        fn into_runtime(self) -> Arc<dyn NorthwardRuntimeApi> {
            Arc::new(self)
        }
    }

    #[async_trait]
    impl NorthwardRuntimeApi for FakeRuntime {
        fn get_point_meta(&self, point_id: i32) -> Option<Arc<PointMeta>> {
            self.points.get(&point_id).map(|e| Arc::clone(e.value()))
        }

        fn get_point_meta_by_path(
            &self,
            _channel_name: &str,
            _device_name: &str,
            _point_key: &str,
        ) -> Option<Arc<PointMeta>> {
            None
        }

        fn subscribe_runtime_delta(&self) -> broadcast::Receiver<RuntimeDelta> {
            broadcast::channel(1).1
        }

        fn list_point_meta(&self) -> Vec<Arc<PointMeta>> {
            self.points.iter().map(|e| Arc::clone(e.value())).collect()
        }
    }

    fn config() -> Arc<OpcuaServerPluginConfig> {
        Arc::new(OpcuaServerPluginConfig {
            bind_addr: "0.0.0.0:14840".to_string(),
            advertised_endpoints: vec!["opc.tcp://gateway.local:14840/".to_string()],
            application_uri: "urn:test:app".to_string(),
            product_uri: "urn:test:product".to_string(),
            namespace_uri: "urn:test:namespace".to_string(),
            ..OpcuaServerPluginConfig::default()
        })
    }

    fn meta(id: i32, ch: &str, dev: &str, key: &str, name: &str) -> PointMeta {
        PointMeta {
            point_id: id,
            channel_id: 1,
            channel_name: ch.into(),
            device_id: 1,
            device_name: dev.into(),
            point_name: name.into(),
            point_key: key.into(),
            data_type: DataType::Float32,
            point_type: DataPointType::Telemetry,
            access_mode: AccessMode::ReadWrite,
            unit: Some("℃".into()),
            min_value: Some(-40.0),
            max_value: Some(120.0),
            transform: Transform {
                transform_scale: Some(2.0),
                transform_offset: Some(0.5),
                transform_negate: true,
                ..Transform::default()
            },
            description: Some("desc".into()),
        }
    }

    #[tokio::test]
    async fn snapshot_uses_config_fallback_when_no_publication() {
        let cfg = config();
        let runtime = FakeRuntime::new()
            .with(meta(1, "ch", "dev", "k1", "p1"))
            .into_runtime();
        let cache = Arc::new(NodeCache::new());
        cache.upsert(1, NodeId::new(1, "ch/dev/k1".to_string()));
        let (_publisher, rx) = channel();

        let inspector =
            OpcuaServerInspector::new(Arc::clone(&cfg), runtime, Arc::clone(&cache), rx);

        let resp = inspector
            .handle(json!({"op": "snapshot"}))
            .await
            .expect("snapshot");
        let parsed: InspectorResponseV1 = serde_json::from_value(resp).unwrap();
        let InspectorResponseV1::Snapshot(snap) = parsed;
        assert_eq!(snap.namespace_index, 1);
        assert_eq!(snap.bind_addr, "0.0.0.0:14840");
        assert_eq!(
            snap.advertised_endpoints,
            vec!["opc.tcp://gateway.local:14840/".to_string()]
        );
        assert!(snap.cert_summary.is_none());
        assert_eq!(snap.materialized.len(), 1);
        let row = &snap.materialized[0];
        assert_eq!(row.node_id, "ns=1;s=ch/dev/k1");
        assert_eq!(row.browse_path, "/Objects/NG-Gateway/ch/dev/k1");
        assert_eq!(row.opcua_data_type, "Float");
        assert_eq!(row.opcua_access_level, "CurrentRead | CurrentWrite");
        assert_eq!(row.point_type, "telemetry");
        assert_eq!(row.access_mode, "read_write");
        assert_eq!(row.wire_data_type, "float32");
        assert_eq!(row.unit.as_deref(), Some("℃"));
        assert!(row.transform_negate);
    }

    #[tokio::test]
    async fn snapshot_uses_published_publication_when_present() {
        let cfg = config();
        let runtime = FakeRuntime::new().into_runtime();
        let cache = Arc::new(NodeCache::new());
        let (publisher, rx) = channel();
        let _guard = RuntimePublicationGuard::publish(
            publisher,
            RuntimePublication {
                namespace_index: 7,
                bind_addr: "0.0.0.0:14840".into(),
                advertised_endpoints: vec!["opc.tcp://gw:4840/".into()],
                cert_summary: None,
            },
        );

        let inspector = OpcuaServerInspector::new(Arc::clone(&cfg), runtime, cache, rx);
        let resp = inspector.handle(json!({"op": "snapshot"})).await.unwrap();
        let parsed: InspectorResponseV1 = serde_json::from_value(resp).unwrap();
        let InspectorResponseV1::Snapshot(snap) = parsed;
        assert_eq!(snap.namespace_index, 7);
        assert_eq!(
            snap.advertised_endpoints,
            vec!["opc.tcp://gw:4840/".to_string()]
        );
        assert!(snap.materialized.is_empty());
    }

    #[tokio::test]
    async fn snapshot_skips_orphan_point_ids_silently() {
        let cfg = config();
        let runtime = FakeRuntime::new().into_runtime();
        let cache = Arc::new(NodeCache::new());
        cache.upsert(99, NodeId::new(1, "ghost".to_string()));
        let (_publisher, rx) = channel();

        let inspector = OpcuaServerInspector::new(Arc::clone(&cfg), runtime, cache, rx);
        let resp = inspector.handle(json!({"op": "snapshot"})).await.unwrap();
        let parsed: InspectorResponseV1 = serde_json::from_value(resp).unwrap();
        let InspectorResponseV1::Snapshot(snap) = parsed;
        assert!(snap.materialized.is_empty());
    }

    #[tokio::test]
    async fn snapshot_orders_rows_by_path_for_determinism() {
        let cfg = config();
        let runtime = FakeRuntime::new()
            .with(meta(1, "b-channel", "dev", "k", "p"))
            .with(meta(2, "a-channel", "dev", "k", "p"))
            .with(meta(3, "a-channel", "dev", "z", "p"))
            .into_runtime();
        let cache = Arc::new(NodeCache::new());
        cache.upsert(1, NodeId::new(1, "b-channel/dev/k".to_string()));
        cache.upsert(3, NodeId::new(1, "a-channel/dev/z".to_string()));
        cache.upsert(2, NodeId::new(1, "a-channel/dev/k".to_string()));
        let (_publisher, rx) = channel();

        let inspector = OpcuaServerInspector::new(cfg, runtime, cache, rx);
        let resp = inspector.handle(json!({"op": "snapshot"})).await.unwrap();
        let InspectorResponseV1::Snapshot(snap) = serde_json::from_value(resp).unwrap();
        let rows: Vec<(String, String)> = snap
            .materialized
            .iter()
            .map(|r| (r.channel_name.clone(), r.point_key.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("a-channel".to_string(), "k".to_string()),
                ("a-channel".to_string(), "z".to_string()),
                ("b-channel".to_string(), "k".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn snapshot_preserves_chinese_segments_in_node_id_and_browse_path() {
        let cfg = config();
        let runtime = FakeRuntime::new()
            .with(meta(1, "通道一", "1号温湿度计", "湿度", "湿度点"))
            .into_runtime();
        let cache = Arc::new(NodeCache::new());
        cache.upsert(1, NodeId::new(1, "通道一/1号温湿度计/湿度".to_string()));
        let (_publisher, rx) = channel();

        let inspector = OpcuaServerInspector::new(cfg, runtime, cache, rx);
        let resp = inspector.handle(json!({"op": "snapshot"})).await.unwrap();
        let InspectorResponseV1::Snapshot(snap) = serde_json::from_value(resp).unwrap();
        let row = &snap.materialized[0];
        assert_eq!(row.node_id, "ns=1;s=通道一/1号温湿度计/湿度");
        assert_eq!(
            row.browse_path,
            "/Objects/NG-Gateway/通道一/1号温湿度计/湿度"
        );
    }

    #[tokio::test]
    async fn rejects_unknown_op_with_validation_failed() {
        let cfg = config();
        let runtime = FakeRuntime::new().into_runtime();
        let cache = Arc::new(NodeCache::new());
        let (_publisher, rx) = channel();
        let inspector = OpcuaServerInspector::new(cfg, runtime, cache, rx);

        let err = inspector
            .handle(json!({"op": "unknown_op"}))
            .await
            .expect_err("unknown op must fail");
        match err {
            NorthwardError::ValidationFailed { .. } => {}
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn capability_id_is_stable_uri_v1() {
        assert!(CAPABILITY_INSPECTOR_V1.starts_with("ng:northward:opcua-server:inspector:"));
        assert!(CAPABILITY_INSPECTOR_V1.ends_with(":v1"));
    }
}
