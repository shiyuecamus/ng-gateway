//! Algorithm registry — write-through cache wrapping WasmAlgorithmHost.
//!
//! Implements [`AiAlgorithmRegistry`] by composing the existing
//! [`WasmAlgorithmHost`] (which owns WASM compilation and execution)
//! with DB-backed persistence via [`AlgorithmRepository`].
//!
//! Data flow:
//! - **Probe**: delegates to `WasmAlgorithmHost::probe_algorithm`
//! - **Install**: host install → DB insert → cache update
//! - **Uninstall**: host delete → DB delete → cache evict
//! - **Query**: cache for list/get, DB for pagination

use crate::algorithm::host::WasmAlgorithmHost;
use bytes::Bytes;
use dashmap::DashMap;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{
        AlgorithmInfo, AlgorithmPageParams, AlgorithmProbeInfo, AlgorithmTestInput,
        AlgorithmTestResult, NewAlgorithm, PageResult,
    },
    AiAlgorithmRegistry,
};
use ng_gateway_repository::AlgorithmRepository;
use sea_orm::IntoActiveModel;
use std::sync::Arc;
use tracing::{info, warn};

/// Algorithm registry — DB-backed write-through cache for WASM algorithms.
pub struct AlgorithmRegistry {
    /// Underlying WASM runtime (compilation + execution).
    host: Arc<WasmAlgorithmHost>,
    /// In-memory cache keyed by algorithm id.
    cache: DashMap<i32, Arc<AlgorithmInfo>>,
}

impl AlgorithmRegistry {
    /// Initialize from DB records and the provided WASM host.
    ///
    /// The host may already contain algorithms discovered from the filesystem
    /// scan. This method reconciles the host state with DB records.
    pub async fn new(host: Arc<WasmAlgorithmHost>) -> Result<Self, AiEngineError> {
        let registry = Self {
            host,
            cache: DashMap::new(),
        };

        let db_algorithms = AlgorithmRepository::list_all()
            .await
            .map_err(|e| AiEngineError::IoError(e.to_string()))?;

        for entity in db_algorithms {
            let info = AlgorithmInfo::from(entity);
            registry.cache.insert(info.id, Arc::new(info));
        }

        info!(
            count = registry.cache.len(),
            "algorithm registry initialized from DB"
        );
        Ok(registry)
    }

    /// Access the underlying WASM host for pipeline execution.
    pub fn host(&self) -> &Arc<WasmAlgorithmHost> {
        &self.host
    }

    /// Get algorithm count from cache.
    pub fn algorithm_count(&self) -> usize {
        self.cache.len()
    }
}

#[async_trait::async_trait]
impl AiAlgorithmRegistry for AlgorithmRegistry {
    async fn probe_algorithm(
        &self,
        wasm_bytes: Bytes,
    ) -> Result<AlgorithmProbeInfo, AiEngineError> {
        self.host.probe_algorithm(wasm_bytes).await
    }

    async fn install_algorithm(&self, wasm_bytes: Bytes) -> Result<AlgorithmInfo, AiEngineError> {
        // 1. Install into WASM host (probe → validate → persist file → register)
        let host_info = self.host.install_algorithm(wasm_bytes).await?;

        // 2. Persist to DB
        let new_algorithm = NewAlgorithm {
            key: host_info.key.clone(),
            name: host_info.name.clone(),
            description: host_info.description.clone(),
            version: host_info.version.clone(),
            module_type: host_info.module_type,
            path: host_info.path.clone(),
            config_schema: host_info.config_schema.clone(),
            size: host_info.size,
            checksum: host_info.checksum.clone(),
        };

        let entity = match AlgorithmRepository::create(
            new_algorithm.into_active_model(),
            None::<&sea_orm::DatabaseConnection>,
        )
        .await
        {
            Ok(entity) => entity,
            Err(e) => {
                let _ = self.host.delete_algorithm(&host_info.key).await;
                return Err(AiEngineError::IoError(format!("DB insert: {e}")));
            }
        };

        // 3. Cache with DB-assigned id
        let info = AlgorithmInfo::from(entity);
        self.cache.insert(info.id, Arc::new(info.clone()));

        info!(
            algorithm_id = info.id,
            algorithm_key = %info.key,
            module_type = ?info.module_type,
            "algorithm installed and persisted"
        );
        Ok(info)
    }

    async fn uninstall_algorithm(&self, algorithm_id: i32) -> Result<(), AiEngineError> {
        let info = self
            .cache
            .get(&algorithm_id)
            .map(|e| Arc::clone(e.value()))
            .ok_or(AiEngineError::AlgorithmError(format!(
                "algorithm {algorithm_id} not found"
            )))?;

        // 1. Delete from WASM host (removes file + runtime entry)
        if let Err(e) = self.host.delete_algorithm(&info.key).await {
            warn!(
                algorithm_id,
                algorithm_key = %info.key,
                error = %e,
                "host delete failed, continuing with DB/cache cleanup"
            );
        }

        // 2. DB delete
        AlgorithmRepository::delete_by_key::<sea_orm::DatabaseConnection>(&info.key, None)
            .await
            .map_err(|e| AiEngineError::IoError(format!("DB delete: {e}")))?;

        // 3. Evict cache
        self.cache.remove(&algorithm_id);

        info!(algorithm_id, algorithm_key = %info.key, "algorithm uninstalled");
        Ok(())
    }

    async fn list_algorithms(&self) -> Result<Vec<AlgorithmInfo>, AiEngineError> {
        Ok(self
            .cache
            .iter()
            .map(|e| e.value().as_ref().clone())
            .collect())
    }

    async fn get_algorithm(
        &self,
        algorithm_id: i32,
    ) -> Result<Option<AlgorithmInfo>, AiEngineError> {
        Ok(self
            .cache
            .get(&algorithm_id)
            .map(|e| e.value().as_ref().clone()))
    }

    async fn page_algorithms(
        &self,
        params: AlgorithmPageParams,
    ) -> Result<PageResult<AlgorithmInfo>, AiEngineError> {
        AlgorithmRepository::page(params)
            .await
            .map_err(|e| AiEngineError::IoError(e.to_string()))
    }

    async fn test_algorithm(
        &self,
        algorithm_id: i32,
        test_input: AlgorithmTestInput,
    ) -> Result<AlgorithmTestResult, AiEngineError> {
        let info = self
            .cache
            .get(&algorithm_id)
            .map(|e| Arc::clone(e.value()))
            .ok_or(AiEngineError::AlgorithmError(format!(
                "algorithm {algorithm_id} not found"
            )))?;

        self.host.test_algorithm(&info.key, test_input).await
    }
}
