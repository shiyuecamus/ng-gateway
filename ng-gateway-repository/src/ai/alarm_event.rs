use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::entities::prelude::{
    AlarmEvent, AlarmEventActiveModel, AlarmEventColumn, AlarmEventModel,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect,
};

/// Repository for AI alarm event persistence.
pub struct AlarmEventRepository;

impl AlarmEventRepository {
    /// Create one alarm event row.
    pub async fn create<C>(
        event: AlarmEventActiveModel,
        db: Option<&C>,
    ) -> StorageResult<AlarmEventModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(event.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(event.insert(&conn).await?)
            }
        }
    }

    /// List recent alarm events by channel.
    pub async fn list_by_channel_id(
        channel_id: i32,
        limit: u64,
    ) -> StorageResult<Vec<AlarmEventModel>> {
        let db = get_db_connection().await?;
        Ok(AlarmEvent::find()
            .filter(AlarmEventColumn::ChannelId.eq(channel_id))
            .order_by_desc(AlarmEventColumn::CreatedAt)
            .limit(limit)
            .all(&db)
            .await?)
    }
}
