use crate::{constants::SYSTEM_ADMIN_ROLE_CODE, domain::prelude::SimpleRole};
use serde::{Deserialize, Serialize};

/// User Role Cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRoleCache {
    pub roles: Vec<SimpleRole>,
}

impl UserRoleCache {
    pub fn is_system_admin(&self) -> bool {
        self.roles
            .iter()
            .any(|role| role.code == SYSTEM_ADMIN_ROLE_CODE)
    }

    pub fn is_any_role(&self, roles: &[&str]) -> bool {
        self.roles
            .iter()
            .any(|role| roles.contains(&role.code.as_str()))
    }
}
