use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{PageResult, RoleInfo, RolePageParams},
    entities::prelude::{
        Relation, RelationColumn, Role, RoleActiveModel, RoleColumn, RoleModel, User, UserModel,
    },
    enums::{common::EntityType, relation::RelationType},
};
use sea_orm::{
    prelude::Expr, sea_query::Query, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait,
    Order, PaginatorTrait, QueryFilter, QueryOrder, QueryTrait,
};

pub struct RoleRepository;

impl RoleRepository {
    pub async fn create<C>(role: RoleActiveModel, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = role.insert(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = role.insert(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn update<C>(role: RoleActiveModel, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = role.update(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = role.update(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = Role::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = Role::delete_by_id(id).exec(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn find_all() -> StorageResult<Vec<RoleInfo>> {
        let db = get_db_connection().await?;
        Ok(Role::find()
            .into_partial_model::<RoleInfo>()
            .all(&db)
            .await?)
    }

    pub async fn page(params: RolePageParams) -> StorageResult<PageResult<RoleInfo>> {
        let db = get_db_connection().await?;
        let query = Role::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(RoleColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.status, |q, status| {
                q.filter(RoleColumn::Status.eq(status))
            })
            .apply_if(params.code.as_ref(), |q, code| {
                q.filter(RoleColumn::Code.like(format!("%{code}%")))
            })
            .apply_if(params.time_range.start_time, |q, start_time| {
                q.filter(RoleColumn::CreatedAt.gte(start_time.naive_utc()))
            })
            .apply_if(params.time_range.end_time, |q, end_time| {
                q.filter(RoleColumn::CreatedAt.lte(end_time.naive_utc()))
            })
            .order_by(RoleColumn::CreatedAt, Order::Desc);
        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let paginator = query
            .into_partial_model::<RoleInfo>()
            .paginate(&db, page_size as u64)
            .fetch_page((page - 1) as u64)
            .await?;

        Ok(PageResult {
            records: paginator,
            total,
            pages: ((total as f64) / (page_size as f64)).ceil() as u32,
            page,
            page_size,
        })
    }

    pub async fn find_user_role_infos(user_id: i32) -> StorageResult<Option<Vec<RoleInfo>>> {
        let db = get_db_connection().await?;
        let roles = Role::find()
            .filter(
                RoleColumn::Id.in_subquery(
                    Query::select()
                        .column(RelationColumn::ToId)
                        .and_where(Expr::column(RelationColumn::FromId).eq(user_id))
                        .and_where(Expr::column(RelationColumn::FromType).eq(EntityType::User))
                        .and_where(Expr::column(RelationColumn::ToType).eq(EntityType::Role))
                        .and_where(Expr::column(RelationColumn::Type).eq(RelationType::Contains))
                        .from(Relation)
                        .to_owned(),
                ),
            )
            .into_partial_model::<RoleInfo>()
            .all(&db)
            .await?;
        Ok(Some(roles))
    }

    pub async fn find_role_info(id: i32) -> StorageResult<Option<RoleInfo>> {
        let db = get_db_connection().await?;
        Ok(Role::find_by_id(id)
            .into_partial_model::<RoleInfo>()
            .one(&db)
            .await?)
    }

    pub async fn find_by_id(id: i32) -> StorageResult<Option<RoleModel>> {
        let db = get_db_connection().await?;
        Ok(Role::find_by_id(id).one(&db).await?)
    }

    pub async fn find_by_id_with_users(
        id: i32,
    ) -> StorageResult<Option<(RoleModel, Vec<UserModel>)>> {
        let db = get_db_connection().await?;
        Ok(Role::find_by_id(id)
            .find_with_related(User)
            .all(&db)
            .await?
            .pop())
    }

    pub async fn count_by_tenant_id() -> StorageResult<u64> {
        let db = get_db_connection().await?;
        Ok(Role::find().count(&db).await?)
    }

    pub async fn exists_by_code(code: &str) -> StorageResult<bool> {
        let db = get_db_connection().await?;

        Ok(Role::find()
            .filter(RoleColumn::Code.eq(code))
            .count(&db)
            .await?
            > 0)
    }

    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(Role::find_by_id(id).count(&db).await? > 0)
    }

    pub async fn exists_by_code_exclude_id(id: i32, code: &str) -> StorageResult<bool> {
        let db = get_db_connection().await?;

        Ok(Role::find()
            .filter(RoleColumn::Id.ne(id))
            .filter(RoleColumn::Code.eq(code))
            .count(&db)
            .await?
            > 0)
    }
}
