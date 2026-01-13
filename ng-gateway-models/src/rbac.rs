use ng_gateway_error::{rbac::RBACError, NGResult};
use std::{future::Future, pin::Pin, sync::Arc};

use crate::domain::prelude::Claims;

/// Trait defining permission rules that can be evaluated against requests
///
/// Permission rules can be combined using logical operations (AND/OR)
/// to create complex authorization policies.
pub trait PermRule: Send + Sync {
    /// Checks if the request satisfies this permission rule
    ///
    /// # Arguments
    /// * `method` - The HTTP method (e.g., "GET", "POST")
    /// * `path` - The API path to protect
    /// * `claims` - The claims of the user
    ///
    /// # Returns
    /// * `NGResult<bool>` - True if permission is granted, False otherwise
    fn check(
        &self,
        method: &str,
        path: &str,
        claims: Arc<Claims>,
    ) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>;

    /// Combines this rule with another rule using AND logic
    ///
    /// Both rules must evaluate to true for the combined rule to pass
    ///
    /// # Arguments
    /// * `rule` - The rule to combine with this rule
    ///
    /// # Returns
    /// * `Box<dyn PermRule>` - A new rule representing the AND combination
    #[inline]
    fn and<R: PermRule + 'static>(self, rule: R) -> Box<dyn PermRule>
    where
        Self: Sized + 'static,
    {
        Box::new(CombinedPermRule {
            rules: vec![Arc::new(self), Arc::new(rule)],
            is_or: false,
        })
    }

    /// Combines this rule with another rule using OR logic
    ///
    /// Either rule evaluating to true will cause the combined rule to pass
    ///
    /// # Arguments
    /// * `rule` - The rule to combine with this rule
    ///
    /// # Returns
    /// * `Box<dyn PermRule>` - A new rule representing the OR combination
    #[inline]
    fn or<R: PermRule + 'static>(self, rule: R) -> Box<dyn PermRule>
    where
        Self: Sized + 'static,
    {
        Box::new(CombinedPermRule {
            rules: vec![Arc::new(self), Arc::new(rule)],
            is_or: true,
        })
    }
}

impl PermRule for Box<dyn PermRule> {
    #[inline]
    fn check(
        &self,
        method: &str,
        path: &str,
        claims: Arc<Claims>,
    ) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>> {
        (**self).check(method, path, claims)
    }
}
/// Basic implementation of a permission rule using a closure
///
/// Provides a flexible way to create custom permission rules
pub struct BasePermRule<F>
where
    F: Fn(
            &str,
            &str,
            Arc<Claims>,
        ) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
        + Send
        + Sync,
{
    /// Function that implements the permission check logic
    check_fn: F,
}

impl<F> BasePermRule<F>
where
    F: Fn(
            &str,
            &str,
            Arc<Claims>,
        ) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
        + Send
        + Sync,
{
    /// Creates a new permission rule with the provided check function
    ///
    /// # Arguments
    /// * `check_fn` - Function that implements the permission check logic
    ///
    /// # Returns
    /// * `Self` - A new BasePermRule instance
    #[inline]
    pub fn new(check_fn: F) -> Self {
        Self { check_fn }
    }
}

impl<F> PermRule for BasePermRule<F>
where
    F: Fn(
            &str,
            &str,
            Arc<Claims>,
        ) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
        + Send
        + Sync,
{
    #[inline]
    fn check(
        &self,
        method: &str,
        path: &str,
        claims: Arc<Claims>,
    ) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>> {
        (self.check_fn)(method, path, claims)
    }
}

/// A rule that combines multiple permission rules using logical operations
///
/// Enables building complex authorization policies by combining simpler rules
pub struct CombinedPermRule {
    /// List of rules to evaluate
    rules: Vec<Arc<dyn PermRule>>,
    /// Whether to use OR logic (true) or AND logic (false)
    is_or: bool,
}

impl PermRule for CombinedPermRule {
    #[inline]
    fn check(
        &self,
        method: &str,
        path: &str,
        claims: Arc<Claims>,
    ) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>> {
        let rules = self.rules.clone();
        let is_or = self.is_or;
        let method = method.to_string();
        let path = path.to_string();
        Box::pin(async move {
            if is_or {
                for rule in &rules {
                    if rule.check(&method, &path, claims.clone()).await? {
                        return Ok(true);
                    }
                }
                Ok(false)
            } else {
                for rule in &rules {
                    if !rule.check(&method, &path, claims.clone()).await? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        })
    }
}
