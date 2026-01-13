use crate::validation::EntityValidator;
use async_trait::async_trait;
use ng_gateway_error::{web::WebError, WebResult};
use ng_gateway_models::{
    entities::{
        prelude::{
            ActionActiveModel, DeviceActiveModel, DriverActiveModel, PluginActiveModel,
            PointActiveModel, RoleActiveModel, UserActiveModel,
        },
        NGEntity,
    },
    enums::common::{EntityType, Operation},
};
use ng_gateway_repository::{
    role::RoleRepository, user::UserRepository, ActionRepository, DeviceRepository,
    DriverRepository, PluginRepository, PointRepository,
};
use tracing::instrument;

/// Duplicate validator supporting type-safe field checking
pub struct EntityDuplicateValidator;

#[async_trait]
impl EntityValidator for EntityDuplicateValidator {
    #[inline]
    fn supported_entity_types(&self) -> Vec<EntityType> {
        vec![
            EntityType::User,
            EntityType::Role,
            EntityType::Driver,
            EntityType::Plugin,
            EntityType::Device,
            EntityType::Point,
            EntityType::Action,
        ]
    }

    #[inline]
    fn supported_operations(&self) -> Vec<Operation> {
        vec![Operation::Create, Operation::Write]
    }

    /// Validates that related entity is enabled
    ///
    /// # Arguments
    /// * `entity` - Entity to validate
    /// * `operation` - Operation to validate
    ///
    /// # Returns
    /// * `WebResult<()>` - Success or status error
    ///
    /// # Errors
    /// * `WebError::BadRequest` - When related entity ID is missing
    /// * `WebError::NotFound` - When related entity is not found
    /// * `WebError::Forbidden` - When related entity is disabled
    #[inline]
    #[instrument(skip(self, entity))]
    async fn validate(&self, entity: &dyn NGEntity, operation: Operation) -> WebResult<()> {
        match entity.entity_type() {
            EntityType::Driver => {
                validate_driver(
                    entity.downcast_ref::<DriverActiveModel>().unwrap(),
                    operation,
                )
                .await?;
            }
            EntityType::Plugin => {
                validate_plugin(
                    entity.downcast_ref::<PluginActiveModel>().unwrap(),
                    operation,
                )
                .await?;
            }
            EntityType::User => {
                validate_user(entity.downcast_ref::<UserActiveModel>().unwrap(), operation).await?;
            }
            EntityType::Role => {
                validate_role(entity.downcast_ref::<RoleActiveModel>().unwrap(), operation).await?;
            }
            EntityType::Device => {
                validate_device(
                    entity.downcast_ref::<DeviceActiveModel>().unwrap(),
                    operation,
                )
                .await?;
            }
            EntityType::Point => {
                validate_point(
                    entity.downcast_ref::<PointActiveModel>().unwrap(),
                    operation,
                )
                .await?;
            }
            EntityType::Action => {
                validate_action(
                    entity.downcast_ref::<ActionActiveModel>().unwrap(),
                    operation,
                )
                .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[inline]
async fn validate_role(role: &RoleActiveModel, operation: Operation) -> WebResult<()> {
    match operation {
        Operation::Create => {
            if RoleRepository::exists_by_code(role.code.to_owned().take().unwrap().as_str()).await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for code".to_string(),
                ));
            }
        }
        Operation::Write => {
            if RoleRepository::exists_by_code_exclude_id(
                role.id.to_owned().take().unwrap(),
                role.code.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for code".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

#[inline]
async fn validate_user(user: &UserActiveModel, operation: Operation) -> WebResult<()> {
    match operation {
        Operation::Create => {
            if UserRepository::exists_by_username_email_phone(
                user.username.to_owned().take().as_deref(),
                user.email.to_owned().take().flatten().as_deref(),
                user.phone.to_owned().take().flatten().as_deref(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for username, email, or phone".to_string(),
                ));
            }
        }
        Operation::Write => {
            if UserRepository::exists_by_username_email_phone_exclude_id(
                user.id.to_owned().take().unwrap(),
                None,
                user.email.to_owned().take().flatten().as_deref(),
                user.phone.to_owned().take().flatten().as_deref(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for username, email, or phone".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

#[inline]
async fn validate_driver(driver: &DriverActiveModel, operation: Operation) -> WebResult<()> {
    match operation {
        Operation::Create => {
            if DriverRepository::exists_by_type_and_version(
                driver.driver_type.to_owned().take().unwrap().as_str(),
                driver.version.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for driver type and version".to_string(),
                ));
            }
        }
        Operation::Write => {
            if DriverRepository::exists_by_type_and_version_exclude_id(
                driver.id.to_owned().take().unwrap(),
                driver.driver_type.to_owned().take().unwrap().as_str(),
                driver.version.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for driver type and version".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

#[inline]
async fn validate_plugin(plugin: &PluginActiveModel, operation: Operation) -> WebResult<()> {
    match operation {
        Operation::Create => {
            if PluginRepository::exists_by_type_and_version(
                plugin.plugin_type.to_owned().take().unwrap().as_str(),
                plugin.version.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for plugin type and version".to_string(),
                ));
            }
        }
        Operation::Write => {
            if PluginRepository::exists_by_type_and_version_exclude_id(
                plugin.id.to_owned().take().unwrap(),
                plugin.plugin_type.to_owned().take().unwrap().as_str(),
                plugin.version.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for plugin type and version".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

async fn validate_device(device: &DeviceActiveModel, operation: Operation) -> WebResult<()> {
    match operation {
        Operation::Create => {
            if DeviceRepository::exists_by_name(device.device_name.to_owned().take().unwrap())
                .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for username, email, or phone".to_string(),
                ));
            }
        }
        Operation::Write => {
            if DeviceRepository::exists_by_name_exclude_id(
                device.id.to_owned().take().unwrap(),
                device.device_name.to_owned().take().unwrap(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for username, email, or phone".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

#[inline]
async fn validate_point(point: &PointActiveModel, operation: Operation) -> WebResult<()> {
    match operation {
        Operation::Create => {
            if PointRepository::exists_by_device_and_key(
                point.device_id.to_owned().take().unwrap(),
                point.key.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for device ID and key".to_string(),
                ));
            }
        }
        Operation::Write => {
            if PointRepository::exists_by_device_and_key_exclude_id(
                point.id.to_owned().take().unwrap(),
                point.device_id.to_owned().take().unwrap(),
                point.key.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for device ID and key".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

#[inline]
async fn validate_action(action: &ActionActiveModel, operation: Operation) -> WebResult<()> {
    match operation {
        Operation::Create => {
            if ActionRepository::exists_by_device_and_command(
                action.device_id.to_owned().take().unwrap(),
                action.command.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for device ID and command".to_string(),
                ));
            }
        }
        Operation::Write => {
            if ActionRepository::exists_by_device_and_command_exclude_id(
                action.id.to_owned().take().unwrap(),
                action.device_id.to_owned().take().unwrap(),
                action.command.to_owned().take().unwrap().as_str(),
            )
            .await?
            {
                return Err(WebError::BadRequest(
                    "duplicate entity for device ID and command".to_string(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}
