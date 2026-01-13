use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{ActionInfo, ActionPageParams, PageResult},
    entities::{
        action::{
            ActiveModel as ActionActiveModel, Column as ActionColumn, Entity as Action,
            Model as ActionModel,
        },
        prelude::{Device, DeviceColumn},
    },
};
use sea_orm::{
    prelude::Expr, sea_query::Query, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait,
    Order, PaginatorTrait, QueryFilter, QueryOrder, QueryTrait,
};

/// Repository for action operations
pub struct ActionRepository;

impl ActionRepository {
    /// Create new action
    pub async fn create<C>(action: ActionActiveModel, db: Option<&C>) -> StorageResult<ActionModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(action.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(action.insert(&conn).await?)
            }
        }
    }

    /// Create new actions (batch)
    pub async fn create_many<C>(
        actions: Vec<ActionActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Action::insert_many(actions)
                .exec_with_returning_many(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Action::insert_many(actions)
                    .exec_with_returning_many(&conn)
                    .await?)
            }
        }
    }

    /// Update existing action
    pub async fn update<C>(action: ActionActiveModel, db: Option<&C>) -> StorageResult<ActionModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(action.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(action.update(&conn).await?)
            }
        }
    }

    /// Delete action by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Action::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Action::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Delete actions by IDs and return deleted rows
    pub async fn delete_by_ids<C>(ids: Vec<i32>, db: Option<&C>) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Action::delete_many()
                .filter(ActionColumn::Id.is_in(ids))
                .exec_with_returning(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Action::delete_many()
                    .filter(ActionColumn::Id.is_in(ids))
                    .exec_with_returning(&conn)
                    .await?)
            }
        }
    }

    /// Find action by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find_by_id(id).one(&conn).await?)
    }

    /// Find action info by ID
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<ActionInfo>> {
        let conn = get_db_connection().await?;
        Ok(Action::find_by_id(id)
            .into_partial_model::<ActionInfo>()
            .one(&conn)
            .await?)
    }

    /// Find actions by IDs
    pub async fn find_by_ids(ids: Vec<i32>) -> StorageResult<Vec<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::Id.is_in(ids))
            .all(&conn)
            .await?)
    }

    /// Find actions by IDs (read-only info)
    pub async fn find_info_by_ids(ids: Vec<i32>) -> StorageResult<Vec<ActionInfo>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::Id.is_in(ids))
            .into_partial_model::<ActionInfo>()
            .all(&conn)
            .await?)
    }

    /// Find all actions
    pub async fn find_all() -> StorageResult<Vec<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .order_by_asc(ActionColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find all actions (read-only info)
    pub async fn find_all_info() -> StorageResult<Vec<ActionInfo>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .order_by_asc(ActionColumn::Id)
            .into_partial_model::<ActionInfo>()
            .all(&conn)
            .await?)
    }

    /// Find actions by device ID
    pub async fn find_by_device_id(device_id: i32) -> StorageResult<Vec<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .order_by_asc(ActionColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find action infos by device ID (read-only info)
    pub async fn find_info_by_device_id(device_id: i32) -> StorageResult<Vec<ActionInfo>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .order_by_asc(ActionColumn::Id)
            .into_partial_model::<ActionInfo>()
            .all(&conn)
            .await?)
    }

    /// Find actions by device IDs
    pub async fn find_by_device_ids<C>(
        device_ids: Vec<i32>,
        db: Option<&C>,
    ) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        let actions = match db {
            Some(conn) => {
                Action::find()
                    .filter(ActionColumn::DeviceId.is_in(device_ids))
                    .order_by_asc(ActionColumn::Id)
                    .all(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                Action::find()
                    .filter(ActionColumn::DeviceId.is_in(device_ids))
                    .order_by_asc(ActionColumn::Id)
                    .all(&conn)
                    .await?
            }
        };
        Ok(actions)
    }

    /// Find action by device ID and name
    pub async fn find_by_device_and_name(
        device_id: i32,
        name: &str,
    ) -> StorageResult<Option<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .filter(ActionColumn::Name.eq(name))
            .one(&conn)
            .await?)
    }

    /// Delete all actions by device ID
    pub async fn delete_by_device_id<C>(
        device_id: i32,
        db: Option<&C>,
    ) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Action::delete_many()
                .filter(ActionColumn::DeviceId.eq(device_id))
                .exec_with_returning(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Action::delete_many()
                    .filter(ActionColumn::DeviceId.eq(device_id))
                    .exec_with_returning(&conn)
                    .await?)
            }
        }
    }

    /// Count actions by device ID
    pub async fn count_by_device_id(device_id: i32) -> StorageResult<u64> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .count(&conn)
            .await?)
    }

    /// Check if action exists by ID
    pub async fn exists(id: i32) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Action::find_by_id(id).count(&conn).await?;
        Ok(count > 0)
    }

    /// Check if action exists by device ID and command
    pub async fn exists_by_device_and_command(
        device_id: i32,
        command: &str,
    ) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .filter(ActionColumn::Command.eq(command))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Check if action exists by device ID and command excluding the given ID
    pub async fn exists_by_device_and_command_exclude_id(
        id: i32,
        device_id: i32,
        command: &str,
    ) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Action::find()
            .filter(ActionColumn::Id.ne(id))
            .filter(ActionColumn::DeviceId.eq(device_id))
            .filter(ActionColumn::Command.eq(command))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Delete all actions by channel ID using subquery
    pub async fn delete_by_channel_id<C>(channel_id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Action::delete_many()
                    .filter(
                        ActionColumn::DeviceId.in_subquery(
                            Query::select()
                                .column(DeviceColumn::Id)
                                .from(Device)
                                .and_where(Expr::col(DeviceColumn::ChannelId).eq(channel_id))
                                .to_owned(),
                        ),
                    )
                    .exec(conn)
                    .await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Action::delete_many()
                    .filter(
                        ActionColumn::DeviceId.in_subquery(
                            Query::select()
                                .column(DeviceColumn::Id)
                                .from(Device)
                                .and_where(Expr::col(DeviceColumn::ChannelId).eq(channel_id))
                                .to_owned(),
                        ),
                    )
                    .exec(&conn)
                    .await?;
            }
        }
        Ok(())
    }

    /// Page query for actions
    pub async fn page(params: ActionPageParams) -> StorageResult<PageResult<ActionInfo>> {
        let db = get_db_connection().await?;

        let query = Action::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(ActionColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.command.as_ref(), |q, command| {
                q.filter(ActionColumn::Command.eq(command))
            })
            .apply_if(params.device_id, |q, device_id| {
                q.filter(ActionColumn::DeviceId.eq(device_id))
            })
            .order_by(ActionColumn::Id, Order::Desc);

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let records = query
            .into_partial_model::<ActionInfo>()
            .paginate(&db, page_size as u64)
            .fetch_page((page - 1) as u64)
            .await?;

        Ok(PageResult {
            records,
            total,
            pages: ((total as f64) / (page_size as f64)).ceil() as u32,
            page,
            page_size,
        })
    }
}
