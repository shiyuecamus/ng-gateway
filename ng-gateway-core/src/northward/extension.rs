//! Host-side extension storage for northward apps.
//!
//! # Why this module exists
//! Northward plugins are loaded as `cdylib` and may run on a separate Tokio runtime.
//! Any attempt to execute host DB code (SeaORM/SQLx) on a plugin runtime thread can
//! crash with errors like "this functionality requires a Tokio context" due to
//! runtime TLS isolation across crate instances.
//!
//! # Best-practice design (方案 B)
//! - Plugins only talk to an `ExtensionStore` trait object.
//! - The host implements the trait as a thin proxy that sends requests to a host-owned actor.
//! - The actor is the only component that touches DB/SeaORM/SQLx.

use async_trait::async_trait;
use futures::{
    channel::{mpsc, oneshot},
    StreamExt,
};
use ng_gateway_repository::AppExtRepository;
use ng_gateway_sdk::{ExtensionStore, NorthwardError, NorthwardResult};
use sea_orm::DatabaseConnection;
use std::{collections::HashMap, sync::Arc};
use tracing::{debug, info_span, Instrument};

/// Host-owned hub that spawns a single extension-store actor.
///
/// Create once (per gateway instance) and then call `client(app_id)` to build a per-app store.
#[derive(Clone)]
pub struct HostExtensionStoreHub {
    tx: mpsc::UnboundedSender<ExtensionStoreMsg>,
}

impl HostExtensionStoreHub {
    /// Spawn a new host extension-store actor.
    ///
    /// # Notes
    /// - The actor runs on the host Tokio runtime.
    /// - The returned hub is cheap to clone and can serve many apps.
    pub fn new(db: DatabaseConnection) -> Self {
        let (tx, mut rx) = mpsc::unbounded::<ExtensionStoreMsg>();
        let span = info_span!("extension-store-actor");
        tokio::spawn(
            async move {
                while let Some(msg) = rx.next().await {
                    // Enter per-request span so host-side per-app log level overrides can apply.
                    let req_span = info_span!("extension-store-request", app_id = msg.app_id);
                    let _enter = req_span.enter();
                    let res = handle_msg(&db, msg.app_id, msg.req).await;
                    if msg.resp.send(res).is_err() {
                        debug!(
                            app_id = msg.app_id,
                            "ExtensionStore response receiver dropped"
                        );
                    }
                }
                debug!("ExtensionStore actor stopped");
            }
            .instrument(span),
        );
        Self { tx }
    }

    /// Create a per-app `ExtensionStore` handle.
    #[inline]
    pub fn client(&self, app_id: i32) -> Arc<dyn ExtensionStore> {
        Arc::new(HostExtensionStore {
            app_id,
            tx: self.tx.clone(),
        })
    }
}

/// Per-app store proxy exposed to plugins.
///
/// This proxy never touches DB. It only sends requests to the host actor.
#[derive(Clone)]
pub struct HostExtensionStore {
    app_id: i32,
    tx: mpsc::UnboundedSender<ExtensionStoreMsg>,
}

#[async_trait]
impl ExtensionStore for HostExtensionStore {
    async fn delete(&self, key: &str) -> NorthwardResult<bool> {
        match self
            .request(ExtensionStoreReq::Delete {
                key: key.to_string(),
            })
            .await?
        {
            ExtensionStoreResp::Bool(v) => Ok(v),
            other => Err(protocol_violation("delete", other)),
        }
    }

    async fn exists(&self, key: &str) -> NorthwardResult<bool> {
        match self
            .request(ExtensionStoreReq::Exists {
                key: key.to_string(),
            })
            .await?
        {
            ExtensionStoreResp::Bool(v) => Ok(v),
            other => Err(protocol_violation("exists", other)),
        }
    }

    async fn keys(&self) -> NorthwardResult<Vec<String>> {
        match self.request(ExtensionStoreReq::Keys).await? {
            ExtensionStoreResp::Keys(v) => Ok(v),
            other => Err(protocol_violation("keys", other)),
        }
    }

    async fn clear(&self) -> NorthwardResult<u64> {
        match self.request(ExtensionStoreReq::Clear).await? {
            ExtensionStoreResp::U64(v) => Ok(v),
            other => Err(protocol_violation("clear", other)),
        }
    }

    async fn len(&self) -> NorthwardResult<usize> {
        match self.request(ExtensionStoreReq::Len).await? {
            ExtensionStoreResp::Usize(v) => Ok(v),
            other => Err(protocol_violation("len", other)),
        }
    }

    async fn get_raw(&self, key: &str) -> NorthwardResult<Option<serde_json::Value>> {
        match self
            .request(ExtensionStoreReq::GetRaw {
                key: key.to_string(),
            })
            .await?
        {
            ExtensionStoreResp::OptValue(v) => Ok(v),
            other => Err(protocol_violation("get_raw", other)),
        }
    }

    async fn set_raw(&self, key: &str, value: serde_json::Value) -> NorthwardResult<()> {
        match self
            .request(ExtensionStoreReq::SetRaw {
                key: key.to_string(),
                value,
            })
            .await?
        {
            ExtensionStoreResp::Unit => Ok(()),
            other => Err(protocol_violation("set_raw", other)),
        }
    }

    async fn get_many_raw(
        &self,
        keys: &[&str],
    ) -> NorthwardResult<HashMap<String, serde_json::Value>> {
        let keys: Vec<String> = keys.iter().map(|s| (*s).to_string()).collect();
        match self.request(ExtensionStoreReq::GetManyRaw { keys }).await? {
            ExtensionStoreResp::Map(v) => Ok(v),
            other => Err(protocol_violation("get_many_raw", other)),
        }
    }
}

impl HostExtensionStore {
    async fn request(&self, req: ExtensionStoreReq) -> NorthwardResult<ExtensionStoreResp> {
        let (tx, rx) = oneshot::channel::<NorthwardResult<ExtensionStoreResp>>();
        self.tx
            .unbounded_send(ExtensionStoreMsg {
                app_id: self.app_id,
                req,
                resp: tx,
            })
            .map_err(|_| NorthwardError::NotConnected)?;

        rx.await.map_err(|_| NorthwardError::RuntimeError {
            reason: "ExtensionStore request cancelled".to_string(),
        })?
    }
}

#[derive(Debug)]
struct ExtensionStoreMsg {
    app_id: i32,
    req: ExtensionStoreReq,
    resp: oneshot::Sender<NorthwardResult<ExtensionStoreResp>>,
}

#[derive(Debug)]
enum ExtensionStoreReq {
    Delete {
        key: String,
    },
    Exists {
        key: String,
    },
    Keys,
    Clear,
    Len,
    GetRaw {
        key: String,
    },
    SetRaw {
        key: String,
        value: serde_json::Value,
    },
    GetManyRaw {
        keys: Vec<String>,
    },
}

#[derive(Debug)]
enum ExtensionStoreResp {
    Unit,
    Bool(bool),
    U64(u64),
    Usize(usize),
    Keys(Vec<String>),
    OptValue(Option<serde_json::Value>),
    Map(HashMap<String, serde_json::Value>),
}

fn protocol_violation(op: &'static str, got: ExtensionStoreResp) -> NorthwardError {
    NorthwardError::RuntimeError {
        reason: format!("ExtensionStore protocol violation: op={op}, got={got:?}"),
    }
}

async fn handle_msg(
    db: &DatabaseConnection,
    app_id: i32,
    req: ExtensionStoreReq,
) -> NorthwardResult<ExtensionStoreResp> {
    match req {
        ExtensionStoreReq::Delete { key } => {
            AppExtRepository::delete::<DatabaseConnection>(app_id, &key, Some(db))
                .await
                .map(ExtensionStoreResp::Bool)
                .map_err(|e| NorthwardError::StorageError {
                    reason: format!("delete failed: app_id={app_id}, key={key}, error={e}"),
                })
        }
        ExtensionStoreReq::Exists { key } => {
            AppExtRepository::exists::<DatabaseConnection>(app_id, &key, Some(db))
                .await
                .map(ExtensionStoreResp::Bool)
                .map_err(|e| NorthwardError::StorageError {
                    reason: format!("exists failed: app_id={app_id}, key={key}, error={e}"),
                })
        }
        ExtensionStoreReq::Keys => {
            AppExtRepository::get_keys::<DatabaseConnection>(app_id, Some(db))
                .await
                .map(ExtensionStoreResp::Keys)
                .map_err(|e| NorthwardError::StorageError {
                    reason: format!("keys failed: app_id={app_id}, error={e}"),
                })
        }
        ExtensionStoreReq::Clear => AppExtRepository::clear::<DatabaseConnection>(app_id, Some(db))
            .await
            .map(ExtensionStoreResp::U64)
            .map_err(|e| NorthwardError::StorageError {
                reason: format!("clear failed: app_id={app_id}, error={e}"),
            }),
        ExtensionStoreReq::Len => AppExtRepository::count::<DatabaseConnection>(app_id, Some(db))
            .await
            .map(ExtensionStoreResp::Usize)
            .map_err(|e| NorthwardError::StorageError {
                reason: format!("len failed: app_id={app_id}, error={e}"),
            }),
        ExtensionStoreReq::GetRaw { key } => {
            let model = AppExtRepository::get_by_key::<DatabaseConnection>(app_id, &key, Some(db))
                .await
                .map_err(|e| NorthwardError::StorageError {
                    reason: format!("get_raw failed: app_id={app_id}, key={key}, error={e}"),
                })?;
            Ok(ExtensionStoreResp::OptValue(model.map(|m| m.value)))
        }
        ExtensionStoreReq::SetRaw { key, value } => {
            AppExtRepository::upsert::<DatabaseConnection>(app_id, &key, value, Some(db))
                .await
                .map(|_| ExtensionStoreResp::Unit)
                .map_err(|e| NorthwardError::StorageError {
                    reason: format!("set_raw failed: app_id={app_id}, key={key}, error={e}"),
                })
        }
        ExtensionStoreReq::GetManyRaw { keys } => {
            let refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            AppExtRepository::get_many::<DatabaseConnection>(app_id, &refs, Some(db))
                .await
                .map(ExtensionStoreResp::Map)
                .map_err(|e| NorthwardError::StorageError {
                    reason: format!("get_many_raw failed: app_id={app_id}, error={e}"),
                })
        }
    }
}
