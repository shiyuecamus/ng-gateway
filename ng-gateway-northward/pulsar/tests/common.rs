use async_trait::async_trait;
use ng_gateway_sdk::{NorthwardRuntimeApi, PointMeta, RuntimeDelta};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct MockRuntime {
    pub points: HashMap<i32, Arc<PointMeta>>,
}

impl MockRuntime {
    pub fn new() -> Self {
        Self {
            points: HashMap::new(),
        }
    }

    pub fn with_point(mut self, point: PointMeta) -> Self {
        self.points.insert(point.point_id, Arc::new(point));
        self
    }
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NorthwardRuntimeApi for MockRuntime {
    fn get_point_meta(&self, point_id: i32) -> Option<Arc<PointMeta>> {
        self.points.get(&point_id).cloned()
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
        let (_tx, rx) = broadcast::channel(1);
        rx
    }

    fn list_point_meta(&self) -> Vec<Arc<PointMeta>> {
        self.points.values().cloned().collect()
    }
}
