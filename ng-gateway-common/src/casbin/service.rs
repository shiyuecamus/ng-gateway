use async_trait::async_trait;
use casbin::{
    function_map::key_match2, DefaultModel, Error as CasbinError, IEnforcer, MgmtApi, RbacApi,
    TryIntoAdapter, TryIntoModel,
};
use ng_gateway_error::{init::InitContextError, NGResult};
use ng_gateway_models::{
    casbin::{CasbinCmd, CasbinResult},
    constants::CASBIN_MODEL,
    CasbinService,
};
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::casbin::adapter::NGCasbinDBAdapter;

#[derive(Clone)]
pub struct NGCasbinService<T: IEnforcer + 'static> {
    enforcer: Arc<RwLock<T>>,
}

impl<T: IEnforcer + 'static> NGCasbinService<T> {
    pub async fn new<M: TryIntoModel, A: TryIntoAdapter>(
        m: M,
        a: A,
    ) -> NGResult<Self, CasbinError> {
        let enforcer = T::new(m, a).await?;
        enforcer
            .get_role_manager()
            .write()
            .matching_fn(Some(key_match2), None);
        Ok(Self {
            enforcer: Arc::new(RwLock::new(enforcer)),
        })
    }
}

#[async_trait]
impl<T: IEnforcer + 'static> CasbinService for NGCasbinService<T> {
    async fn init(db: DatabaseConnection) -> NGResult<Arc<Self>, InitContextError> {
        let adapter = NGCasbinDBAdapter::new(db);
        let model = DefaultModel::from_str(CASBIN_MODEL)
            .await
            .map_err(|e| InitContextError::Primitive(format!("Casbin model init failed: {e}")))?;
        let enforcer = T::new(model, adapter).await.map_err(|e| {
            InitContextError::Primitive(format!("Casbin enforcer init failed: {e}"))
        })?;
        enforcer
            .get_role_manager()
            .write()
            .matching_fn(Some(key_match2), None);
        Ok(Arc::new(Self {
            enforcer: Arc::new(RwLock::new(enforcer)),
        }))
    }

    async fn handle_cmd(&self, cmd: CasbinCmd) -> NGResult<CasbinResult, CasbinError> {
        let mut lock = self.enforcer.write().await;
        let result = match cmd {
            CasbinCmd::Enforce(policy) => lock.enforce(policy).map(CasbinResult::Enforce),
            CasbinCmd::AddPolicy(policy) => {
                lock.add_policy(policy).await.map(CasbinResult::AddPolicy)
            }
            CasbinCmd::AddPolicies(policy) => lock
                .add_policies(policy)
                .await
                .map(CasbinResult::AddPolicies),
            CasbinCmd::AddNamedPolicy(ptype, policy) => lock
                .add_named_policy(&ptype, policy)
                .await
                .map(CasbinResult::AddNamedPolicy),
            CasbinCmd::AddNamedPolicies(ptype, policy) => lock
                .add_named_policies(&ptype, policy)
                .await
                .map(CasbinResult::AddNamedPolicies),
            CasbinCmd::AddGroupingPolicy(policy) => lock
                .add_grouping_policy(policy)
                .await
                .map(CasbinResult::AddGroupingPolicy),
            CasbinCmd::AddGroupingPolicies(policy) => lock
                .add_grouping_policies(policy)
                .await
                .map(CasbinResult::AddGroupingPolicies),
            CasbinCmd::AddNamedGroupingPolicy(ptype, policy) => lock
                .add_named_grouping_policy(&ptype, policy)
                .await
                .map(CasbinResult::AddNamedGroupingPolicy),
            CasbinCmd::AddNamedGroupingPolicies(ptype, policy) => lock
                .add_named_grouping_policies(&ptype, policy)
                .await
                .map(CasbinResult::AddNamedGroupingPolicies),
            CasbinCmd::RemoveNamedPolicy(ptype, policy) => lock
                .remove_named_policy(&ptype, policy)
                .await
                .map(CasbinResult::RemoveNamedPolicy),
            CasbinCmd::RemoveNamedPolicies(ptype, policy) => lock
                .remove_named_policies(&ptype, policy)
                .await
                .map(CasbinResult::RemoveNamedPolicies),
            CasbinCmd::RemoveGroupingPolicy(policy) => lock
                .remove_grouping_policy(policy)
                .await
                .map(CasbinResult::RemoveGroupingPolicy),
            CasbinCmd::RemoveGroupingPolicies(policy) => lock
                .remove_grouping_policies(policy)
                .await
                .map(CasbinResult::RemoveGroupingPolicies),
            CasbinCmd::RemoveNamedGroupingPolicy(ptype, policy) => lock
                .remove_named_grouping_policy(&ptype, policy)
                .await
                .map(CasbinResult::RemoveNamedGroupingPolicy),
            CasbinCmd::RemoveNamedGroupingPolicies(ptype, policy) => lock
                .remove_named_grouping_policies(&ptype, policy)
                .await
                .map(CasbinResult::RemoveNamedGroupingPolicies),
            CasbinCmd::RemovePolicy(policy) => lock
                .remove_policy(policy)
                .await
                .map(CasbinResult::RemovePolicy),
            CasbinCmd::RemovePolicies(policy) => lock
                .remove_policies(policy)
                .await
                .map(CasbinResult::RemovePolicies),
            CasbinCmd::RemoveFilteredNamedPolicy(ptype, idx, policy) => lock
                .remove_filtered_named_policy(&ptype, idx, policy)
                .await
                .map(CasbinResult::RemoveFilteredNamedPolicy),
            CasbinCmd::RemoveFilteredNamedGroupingPolicy(ptype, idx, policy) => lock
                .remove_filtered_named_grouping_policy(&ptype, idx, policy)
                .await
                .map(CasbinResult::RemoveFilteredNamedGroupingPolicy),
            CasbinCmd::AddRoleForUser(user, roles, domain) => lock
                .add_role_for_user(&user, &roles, domain.as_deref())
                .await
                .map(CasbinResult::AddRoleForUser),
            CasbinCmd::AddRolesForUser(user, roles, domain) => lock
                .add_roles_for_user(&user, roles, domain.as_deref())
                .await
                .map(CasbinResult::AddRolesForUser),
            CasbinCmd::DeleteRoleForUser(user, roles, domain) => lock
                .delete_role_for_user(&user, &roles, domain.as_deref())
                .await
                .map(CasbinResult::DeleteRoleForUser),
            CasbinCmd::DeleteRolesForUser(user, domain) => lock
                .delete_roles_for_user(&user, domain.as_deref())
                .await
                .map(CasbinResult::DeleteRolesForUser),
            CasbinCmd::GetImplicitRolesForUser(name, domain) => {
                Ok(CasbinResult::GetImplicitRolesForUser(
                    lock.get_implicit_roles_for_user(&name, domain.as_deref()),
                ))
            }
            CasbinCmd::GetImplicitPermissionsForUser(name, domain) => {
                Ok(CasbinResult::GetImplicitPermissionsForUser(
                    lock.get_implicit_permissions_for_user(&name, domain.as_deref()),
                ))
            }
            CasbinCmd::HasRole(user, role, domain) => {
                let user_roles = lock.get_roles_for_user(&user, domain.as_deref());
                Ok(CasbinResult::HasRole(user_roles.contains(&role)))
            }
            CasbinCmd::HasRoles(user, roles, domain) => {
                let user_roles = lock.get_roles_for_user(&user, domain.as_deref());
                let has_all_roles = roles.iter().all(|role| user_roles.contains(role));
                Ok(CasbinResult::HasRoles(has_all_roles))
            }
            CasbinCmd::HasAnyRole(user, roles, domain) => {
                let user_roles = lock.get_roles_for_user(&user, domain.as_deref());
                let has_any_role = roles.iter().any(|role| user_roles.contains(role));
                Ok(CasbinResult::HasAnyRole(has_any_role))
            }
        };
        drop(lock);
        result
    }
}
