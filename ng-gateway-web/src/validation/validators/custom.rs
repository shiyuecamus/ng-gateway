use crate::validation::EntityValidator;
use async_trait::async_trait;
use ng_gateway_error::WebResult;
use ng_gateway_models::{
    entities::NGEntity,
    enums::common::{EntityType, Operation},
};
use std::{future::Future, pin::Pin, sync::Arc};
use tracing::instrument;

/// Type alias for the custom validation closure.
/// The closure takes an entity and a validation context, and returns a future resolving to `WebResult<()>`.
/// The returned Future can borrow from the input references.
type CustomValidationLogic = dyn for<'a> Fn(
        &'a dyn NGEntity,
        Operation,
    ) -> Pin<Box<dyn Future<Output = WebResult<()>> + Send + 'a>>
    + Send
    + Sync;

/// A custom validator that allows defining validation logic via a closure.
pub struct CustomValidator {
    /// Entity types supported by this validator.
    supported_entity_types: Vec<EntityType>,
    /// Operations supported by this validator.
    supported_operations: Vec<Operation>,
    /// The custom validation logic closure.
    logic: Arc<CustomValidationLogic>,
    /// A descriptive name for the validator, useful for debugging.
    name: String,
}

impl CustomValidator {
    /// Creates a new `CustomValidator`.
    ///
    /// # Arguments
    ///
    /// * `name` - A descriptive name for the validator.
    /// * `supported_entity_types` - A list of entity types this validator applies to.
    /// * `supported_operations` - A list of operations this validator applies to.
    /// * `logic` - The closure that implements the custom validation logic.
    ///
    /// # Returns
    ///
    /// * `Self` - A new instance of `CustomValidator`.
    pub fn new(
        name: String,
        supported_entity_types: Vec<EntityType>,
        supported_operations: Vec<Operation>,
        logic: impl for<'a> Fn(
                &'a dyn NGEntity,
                Operation,
            ) -> Pin<Box<dyn Future<Output = WebResult<()>> + Send + 'a>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            name,
            supported_entity_types,
            supported_operations,
            logic: Arc::new(logic),
        }
    }
}

#[async_trait]
impl EntityValidator for CustomValidator {
    // The `name` method was removed as it's not part of the EntityValidator trait.
    // The `instrument` macro below was adjusted accordingly.

    #[inline]
    fn supported_entity_types(&self) -> Vec<EntityType> {
        self.supported_entity_types.clone()
    }

    #[inline]
    fn supported_operations(&self) -> Vec<Operation> {
        self.supported_operations.clone()
    }

    /// Executes the custom validation logic.
    ///
    /// # Arguments
    ///
    /// * `entity` - The entity to validate.
    /// * `operation` - The operation to validate.
    ///
    /// # Returns
    ///
    /// * `WebResult<()>` - Ok if validation passes, or an error if it fails.
    #[instrument(skip(self, entity, operation), fields(validator_name = %self.name))]
    async fn validate(&self, entity: &dyn NGEntity, operation: Operation) -> WebResult<()> {
        (self.logic)(entity, operation).await
    }
}
