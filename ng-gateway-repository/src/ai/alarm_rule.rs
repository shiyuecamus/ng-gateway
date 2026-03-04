use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::entities::prelude::{
    AlarmRule, AlarmRuleActiveModel, AlarmRuleColumn, AlarmRuleModel,
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

/// Repository for AI alarm rule persistence.
pub struct AlarmRuleRepository;

impl AlarmRuleRepository {
    /// List alarm rules by `pipeline_id`, ordered by `rule_order`.
    pub async fn list_by_pipeline_id(pipeline_id: i32) -> StorageResult<Vec<AlarmRuleModel>> {
        let db = get_db_connection().await?;
        Ok(AlarmRule::find()
            .filter(AlarmRuleColumn::PipelineId.eq(pipeline_id))
            .order_by_asc(AlarmRuleColumn::RuleOrder)
            .all(&db)
            .await?)
    }

    /// Replace all alarm rules for one pipeline in a single connection context.
    pub async fn replace_by_pipeline_id<C>(
        pipeline_id: i32,
        rules: Vec<AlarmRuleActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                AlarmRule::delete_many()
                    .filter(AlarmRuleColumn::PipelineId.eq(pipeline_id))
                    .exec(conn)
                    .await?;
                if !rules.is_empty() {
                    AlarmRule::insert_many(rules).exec(conn).await?;
                }
            }
            None => {
                let conn = get_db_connection().await?;
                AlarmRule::delete_many()
                    .filter(AlarmRuleColumn::PipelineId.eq(pipeline_id))
                    .exec(&conn)
                    .await?;
                if !rules.is_empty() {
                    AlarmRule::insert_many(rules).exec(&conn).await?;
                }
            }
        }
        Ok(())
    }

    /// Delete all alarm rules for one pipeline.
    pub async fn delete_by_pipeline_id(pipeline_id: i32) -> StorageResult<()> {
        let db = get_db_connection().await?;
        AlarmRule::delete_many()
            .filter(AlarmRuleColumn::PipelineId.eq(pipeline_id))
            .exec(&db)
            .await?;
        Ok(())
    }
}
