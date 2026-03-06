use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{AlarmEventInfo, AlarmEventPageParams, PageResult},
    entities::prelude::{AlarmEvent, AlarmEventActiveModel, AlarmEventColumn, AlarmEventModel},
    enums::ai::AlarmEventStatus,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect,
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

    /// Paginated query with optional filters.
    pub async fn page(params: &AlarmEventPageParams) -> StorageResult<PageResult<AlarmEventInfo>> {
        let db = get_db_connection().await?;
        let mut query = AlarmEvent::find().order_by_desc(AlarmEventColumn::CreatedAt);

        if let Some(channel_id) = params.channel_id {
            query = query.filter(AlarmEventColumn::ChannelId.eq(channel_id));
        }
        if let Some(pipeline_id) = params.pipeline_id {
            query = query.filter(AlarmEventColumn::PipelineId.eq(pipeline_id));
        }
        if let Some(ref alarm_type) = params.alarm_type {
            query = query.filter(AlarmEventColumn::AlarmType.eq(*alarm_type));
        }
        if let Some(ref severity) = params.severity {
            query = query.filter(AlarmEventColumn::Severity.eq(*severity));
        }
        if let Some(ref status) = params.status {
            query = query.filter(AlarmEventColumn::Status.eq(*status));
        }
        if let Some(ref start) = params.time_range.start_time {
            query = query.filter(AlarmEventColumn::CreatedAt.gte(*start));
        }
        if let Some(ref end) = params.time_range.end_time {
            query = query.filter(AlarmEventColumn::CreatedAt.lte(*end));
        }

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let paginator = query
            .into_partial_model::<AlarmEventInfo>()
            .paginate(&db, page_size);
        let total = paginator.num_items().await?;
        let records = paginator.fetch_page(page.saturating_sub(1)).await?;

        Ok(PageResult {
            records,
            total,
            pages: if page_size > 0 {
                total.div_ceil(page_size)
            } else {
                0
            },
            page,
            page_size,
        })
    }

    /// Get a single alarm event by ID.
    pub async fn get_by_id(id: i32) -> StorageResult<Option<AlarmEventModel>> {
        let db = get_db_connection().await?;
        Ok(AlarmEvent::find_by_id(id).one(&db).await?)
    }

    /// Update alarm event status (acknowledge / close).
    pub async fn update_status(
        id: i32,
        status: AlarmEventStatus,
    ) -> StorageResult<AlarmEventModel> {
        let db = get_db_connection().await?;
        let now = chrono::Utc::now();

        let mut model: AlarmEventActiveModel = AlarmEvent::find_by_id(id)
            .one(&db)
            .await?
            .ok_or_else(|| sea_orm::DbErr::RecordNotFound(format!("alarm_event {id}")))?
            .into();

        model.status = ActiveValue::Set(status);
        model.updated_at = ActiveValue::Set(now);

        match status {
            AlarmEventStatus::Acked => {
                model.acked_at = ActiveValue::Set(Some(now));
            }
            AlarmEventStatus::Closed => {
                model.closed_at = ActiveValue::Set(Some(now));
            }
            _ => {}
        }

        Ok(model.update(&db).await?)
    }
}
