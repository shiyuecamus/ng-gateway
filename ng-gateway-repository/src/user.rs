use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{PageResult, UserInfo, UserPageParams},
    entities::prelude::{Role, RoleModel, User, UserActiveModel, UserColumn, UserModel},
    enums::common::Status,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, Order, PaginatorTrait,
    QueryFilter, QueryOrder, QueryTrait,
};

pub struct UserRepository;

impl UserRepository {
    pub async fn create<C>(user: UserActiveModel, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = user.insert(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = user.insert(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn update<C>(user: UserActiveModel, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = user.update(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = user.update(&db).await?;
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
                let _ = User::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = User::delete_by_id(id).exec(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn find_all() -> StorageResult<Vec<UserInfo>> {
        let db = get_db_connection().await?;
        Ok(User::find()
            .into_partial_model::<UserInfo>()
            .all(&db)
            .await?)
    }

    pub async fn find_by_id_with_roles(
        id: i32,
    ) -> StorageResult<Option<(UserModel, Vec<RoleModel>)>> {
        let db = get_db_connection().await?;

        Ok(User::find()
            .find_with_related(Role)
            .filter(UserColumn::Id.eq(id))
            .all(&db)
            .await?
            .pop())
    }

    pub async fn page(params: UserPageParams) -> StorageResult<PageResult<UserInfo>> {
        let db = get_db_connection().await?;
        let query = User::find()
            .apply_if(params.username.as_ref(), |q, username| {
                q.filter(UserColumn::Username.like(format!("%{username}%")))
            })
            .apply_if(params.status, |q, status| {
                q.filter(UserColumn::Status.eq(status))
            })
            .apply_if(params.email.as_ref(), |q, email| {
                q.filter(UserColumn::Email.like(format!("%{email}%")))
            })
            .apply_if(params.time_range.start_time, |q, start_time| {
                q.filter(UserColumn::CreatedAt.gte(start_time.naive_utc()))
            })
            .apply_if(params.time_range.end_time, |q, end_time| {
                q.filter(UserColumn::CreatedAt.lte(end_time.naive_utc()))
            })
            .order_by(UserColumn::CreatedAt, Order::Desc);
        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let paginator = query
            .into_partial_model::<UserInfo>()
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

    pub async fn find_user_info(id: i32) -> StorageResult<Option<UserInfo>> {
        let db = get_db_connection().await?;
        Ok(User::find_by_id(id)
            .into_partial_model::<UserInfo>()
            .one(&db)
            .await?)
    }

    pub async fn find_by_id(id: i32) -> StorageResult<Option<UserModel>> {
        let db = get_db_connection().await?;
        Ok(User::find_by_id(id).one(&db).await?)
    }

    pub async fn find_by_username(username: &str) -> StorageResult<Option<UserModel>> {
        let db = get_db_connection().await?;
        Ok(User::find()
            .filter(UserColumn::Username.eq(username))
            .one(&db)
            .await?)
    }

    pub async fn find_by_username_and_status(
        username: &str,
        status: Status,
    ) -> StorageResult<Option<UserModel>> {
        let db = get_db_connection().await?;
        Ok(User::find()
            .filter(UserColumn::Username.eq(username))
            .filter(UserColumn::Status.eq(status))
            .one(&db)
            .await?)
    }

    pub async fn count_by_tenant_id() -> StorageResult<u64> {
        let db = get_db_connection().await?;
        Ok(User::find().count(&db).await?)
    }

    pub async fn exists_by_username_email_phone(
        username: Option<&str>,
        email: Option<&str>,
        phone: Option<&str>,
    ) -> StorageResult<bool> {
        let db = get_db_connection().await?;

        if let Some(condition) = Self::build_username_email_phone_condition(username, email, phone)
        {
            Ok(User::find().filter(condition).count(&db).await? > 0)
        } else {
            Ok(false)
        }
    }

    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(User::find_by_id(id).count(&db).await? > 0)
    }

    pub async fn exists_by_username_email_phone_exclude_id(
        id: i32,
        username: Option<&str>,
        email: Option<&str>,
        phone: Option<&str>,
    ) -> StorageResult<bool> {
        let db = get_db_connection().await?;

        if let Some(condition) = Self::build_username_email_phone_condition(username, email, phone)
        {
            Ok(User::find()
                .filter(UserColumn::Id.ne(id))
                .filter(condition)
                .count(&db)
                .await?
                > 0)
        } else {
            Ok(false)
        }
    }

    fn build_username_email_phone_condition(
        username: Option<&str>,
        email: Option<&str>,
        phone: Option<&str>,
    ) -> Option<Condition> {
        let mut conditions = vec![];

        if let Some(username) = username {
            conditions.push(UserColumn::Username.eq(username));
        }

        if let Some(email) = email {
            conditions.push(UserColumn::Email.eq(email));
        }

        if let Some(phone) = phone {
            conditions.push(UserColumn::Phone.eq(phone));
        }

        if conditions.is_empty() {
            None
        } else {
            Some(
                conditions
                    .into_iter()
                    .fold(Condition::any(), |acc, condition| acc.add(condition)),
            )
        }
    }
}
