pub mod manager;
pub mod prelude;
pub mod validators;

use async_trait::async_trait;
use ng_gateway_error::WebResult;
use ng_gateway_models::entities::NGEntity;
use ng_gateway_models::enums::common::{EntityType, Operation};
use std::sync::Arc;

/// Core trait for entity validators
#[async_trait]
pub trait EntityValidator: Send + Sync {
    /// Returns entity types supported by this validator
    fn supported_entity_types(&self) -> Vec<EntityType>;

    /// Returns operations supported by this validator
    fn supported_operations(&self) -> Vec<Operation>;

    /// Performs validation on the entity
    ///
    /// # Arguments
    /// * `entity` - Entity to validate
    /// * `operation` - Operation to validate
    ///
    /// # Returns
    /// * `WebResult<()>` - Success or validation error
    async fn validate(&self, entity: &dyn NGEntity, operation: Operation) -> WebResult<()>;

    /// Checks if this validator applies to the given entity type and operation
    ///
    /// # Arguments
    /// * `entity_type` - Entity type to check
    /// * `operation` - Operation to check
    ///
    /// # Returns
    /// * `bool` - True if validator is applicable
    fn is_applicable(&self, entity_type: &EntityType, operation: &Operation) -> bool {
        self.supported_entity_types().contains(entity_type)
            && self.supported_operations().contains(operation)
    }
}
