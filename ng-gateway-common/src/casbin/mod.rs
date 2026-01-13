pub mod adapter;
pub mod service;

use actix_web::http::Method;
use async_trait::async_trait;
use ng_gateway_error::{rbac::RBACError, NGResult};
use ng_gateway_models::{domain::prelude::Claims, rbac::PermRule, PermChecker};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

type BoxedPermRule = Box<dyn PermRule>;

/// Permission checker for API routes
///
/// Manages the registration and evaluation of permission rules for different API endpoints.
#[derive(Default)]
pub struct NGPermChecker {
    rules: Arc<RwLock<HashMap<(String, String), BoxedPermRule>>>,
}

impl NGPermChecker {
    /// Creates a new permission checker with an empty rule set
    ///
    /// # Returns
    /// * `Self` - A new PermChecker instance
    #[inline]
    pub fn new() -> Self {
        Self {
            rules: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl PermChecker for NGPermChecker {
    fn init() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Registers a permission rule for a specific HTTP method and path
    ///
    /// # Arguments
    /// * `method` - The HTTP method (e.g., "GET", "POST")
    /// * `path` - The API path to protect
    /// * `rule` - The permission rule to apply
    ///
    /// # Returns
    /// * `NGResult<()>` - Ok if registration was successful, or Error if key already exists
    #[inline]
    async fn register<R: PermRule + 'static>(
        &self,
        method: Method,
        path: String,
        rule: R,
    ) -> NGResult<(), RBACError> {
        let key = (method.as_str().to_string(), path);
        let mut rules = self.rules.write().await;

        // Check for duplicate key
        if rules.contains_key(&key) {
            return Err(RBACError::RuleExists {
                method: key.0.clone(),
                path: key.1.clone(),
            });
        }

        rules.insert(key, Box::new(rule));
        Ok(())
    }

    /// Checks if a request passes the registered permission rules
    ///
    /// # Arguments
    /// * `method` - The HTTP method (e.g., "GET", "POST")
    /// * `path` - The API path to protect
    /// * `claims` - The claims of the user
    ///
    /// # Returns
    /// * `NGResult<bool>` - True if permission is granted, False otherwise
    #[inline]
    async fn check(
        &self,
        method: &str,
        path: &str,
        claims: Arc<Claims>,
    ) -> NGResult<bool, RBACError> {
        let key = (method.to_string(), path.to_string());

        if let Some(rule) = self.rules.read().await.get(&key) {
            return rule.check(method, path, claims).await;
        }

        // default allowed
        Ok(true)
    }
}
