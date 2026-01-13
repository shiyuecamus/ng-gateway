use super::{manager::ValidationManager, validators::duplicate::EntityDuplicateValidator};
use std::sync::Arc;

/// Create a default validation manager with pre-registered validators
///
/// # Returns
/// * `ValidationManager` - The default validation manager
pub fn create_default_manager() -> ValidationManager {
    let mut manager = ValidationManager::new();
    manager.register(Arc::new(EntityDuplicateValidator));

    // 示例：注册一个自定义验证器，用于验证用户实体的创建操作
    // manager.register(Arc::new(CustomValidator::new(
    //     "CustomUserCreateValidator".to_string(),
    //     vec![EntityType::User],
    //     vec![Operation::Create],
    //     |entity, ctx| {
    //         Box::pin(async move {
    //             // 在这里实现您的自定义验证逻辑
    //             // 例如，检查用户实体的某个特定字段是否符合要求
    //             // 如果验证失败，返回 Err(NGError::WebError(WebError::BadRequest("Custom validation failed".to_string())))
    //             // 此处仅为示例，直接返回 Ok(())
    //             println!(
    //                 "Running custom validation for entity: {:?}, operation: {:?}",
    //                 entity.entity_type(),
    //                 ctx.operation
    //             );
    //             Ok(())
    //         })
    //     },
    // )));

    manager
}
