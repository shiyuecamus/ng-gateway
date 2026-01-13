use super::*;
use tracing::{debug, instrument};

/// Manager for entity validators that handles registration and execution
#[derive(Default)]
pub struct ValidationManager {
    /// Registered validators
    validators: Vec<Arc<dyn EntityValidator>>,
}

impl ValidationManager {
    /// Creates a new validation manager
    pub fn new() -> Self {
        Self {
            validators: Vec::new(),
        }
    }

    /// Registers a validator with the manager
    ///
    /// # Arguments
    /// * `validator` - Validator to register
    ///
    /// # Returns
    /// * `&mut Self` - Self reference for method chaining
    pub fn register(&mut self, validator: Arc<dyn EntityValidator>) -> &mut Self {
        self.validators.push(validator);
        self
    }

    /// Executes all applicable validators for the entity
    ///
    /// # Arguments
    /// * `entity` - Entity to validate
    /// * `operation` - Operation to validate
    ///
    /// # Returns
    /// * `WebResult<()>` - Success or validation error
    #[instrument(skip(self, entity))]
    pub async fn validate(&self, entity: &dyn NGEntity, operation: Operation) -> WebResult<()> {
        let entity_type = entity.entity_type();

        debug!("Validating {:?} operation on {:?}", operation, entity_type);

        for validator in &self.validators {
            if validator.is_applicable(&entity_type, &operation) {
                validator.validate(entity, operation).await?;
            }
        }

        Ok(())
    }
}
