use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::MenuInfo,
    entities::prelude::{Menu, MenuActiveModel, MenuColumn, MenuModel, Relation, RelationColumn},
    enums::{
        common::{EntityType, Status},
        relation::RelationType,
    },
};
use sea_orm::{
    prelude::Expr,
    sea_query::{ExprTrait, Query},
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder,
};

pub struct MenuRepository;

impl MenuRepository {
    pub async fn create<C>(menu: MenuActiveModel, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = menu.insert(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = menu.insert(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn update<C>(menu: MenuActiveModel, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let _ = menu.update(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = menu.update(&db).await?;
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
                let _ = Menu::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let db = get_db_connection().await?;
                let _ = Menu::delete_by_id(id).exec(&db).await?;
            }
        }
        Ok(())
    }

    pub async fn find_all() -> StorageResult<Vec<MenuModel>> {
        let db = get_db_connection().await?;
        Ok(Menu::find().order_by_asc(MenuColumn::Sort).all(&db).await?)
    }

    pub async fn find_all_with_status(status: Status) -> StorageResult<Vec<MenuInfo>> {
        let db = get_db_connection().await?;
        Ok(Menu::find()
            .filter(MenuColumn::Status.eq(status))
            .order_by_asc(MenuColumn::Sort)
            .into_partial_model::<MenuInfo>()
            .all(&db)
            .await?)
    }

    pub async fn find_all_with_roles_and_status(
        roles: Vec<i32>,
        status: Status,
    ) -> StorageResult<Vec<MenuInfo>> {
        let db = get_db_connection().await?;
        let menus = Menu::find()
            .filter(
                Condition::any().add(
                    MenuColumn::Id.in_subquery(
                        Query::select()
                            .column(RelationColumn::ToId)
                            .and_where(Expr::column(RelationColumn::FromId).is_in(roles))
                            .and_where(Expr::column(RelationColumn::FromType).eq(EntityType::Role))
                            .and_where(Expr::column(RelationColumn::ToType).eq(EntityType::Menu))
                            .and_where(
                                Expr::column(RelationColumn::Type).eq(RelationType::Contains),
                            )
                            .from(Relation)
                            .to_owned(),
                    ),
                ),
            )
            .filter(MenuColumn::Status.eq(status))
            .into_partial_model::<MenuInfo>()
            .all(&db)
            .await?;
        Ok(menus)
    }

    pub async fn find_by_id(id: i32) -> StorageResult<Option<MenuModel>> {
        let db = get_db_connection().await?;
        Ok(Menu::find_by_id(id).one(&db).await?)
    }

    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(Menu::find_by_id(id).count(&db).await? > 0)
    }
}
