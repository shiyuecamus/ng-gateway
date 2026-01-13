use crate::initializer::{InitContext, NGInitializer};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_APP_SUB_ORDER,
    create_table = create_app_sub_table,
    create_indexes = create_app_sub_indexes,
))]
pub enum AppSub {
    Table,
    Id,
    AppId,
    AllDevices,
    DeviceIds,
    Priority,
}

fn create_app_sub_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(AppSub::Table)
        .if_not_exists()
        .col(pk_auto(AppSub::Id))
        .col(
            ColumnDef::new(AppSub::AppId)
                .integer()
                .not_null()
                .comment("FK: northward_app.id"),
        )
        .col(
            ColumnDef::new(AppSub::AllDevices)
                .boolean()
                .not_null()
                .default(false)
                .comment("All devices"),
        )
        .col(
            ColumnDef::new(AppSub::DeviceIds)
                .json_binary()
                .comment("Device ids"),
        )
        .col(
            ColumnDef::new(AppSub::Priority)
                .integer()
                .not_null()
                .default(0)
                .comment("Priority"),
        )
        .to_owned()
}

fn create_app_sub_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![Index::create()
        .name("idx_nw_sub_app")
        .table(AppSub::Table)
        .col(AppSub::AppId)
        .to_owned()])
}
