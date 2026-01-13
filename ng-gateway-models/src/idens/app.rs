use crate::{
    enums::common::Status,
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_APP_ORDER,
    create_table = create_app_table,
    create_indexes = create_app_indexes,
))]
pub enum App {
    Table,
    Id,
    PluginId,
    Name,
    Description,
    Config,
    RetryPolicy,
    QueuePolicy,
    Status,
    CreatedAt,
    UpdatedAt,
}

fn create_app_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(App::Table)
        .if_not_exists()
        .col(pk_auto(App::Id))
        .col(
            ColumnDef::new(App::PluginId)
                .integer()
                .not_null()
                .comment("FK: plugin.id"),
        )
        .col(
            ColumnDef::new(App::Name)
                .string_len(64)
                .not_null()
                .comment("Northward app name"),
        )
        .col(
            ColumnDef::new(App::Description)
                .string_len(512)
                .comment("Northward app description"),
        )
        .col(
            ColumnDef::new(App::Config)
                .json_binary()
                .not_null()
                .comment("Adapter config JSON"),
        )
        .col(
            ColumnDef::new(App::RetryPolicy)
                .json_binary()
                .not_null()
                .comment("Retry policy JSON"),
        )
        .col(
            ColumnDef::new(App::QueuePolicy)
                .json_binary()
                .not_null()
                .comment("Batch policy JSON"),
        )
        .col(
            ColumnDef::new(App::Status)
                .small_integer()
                .default(Status::Enabled)
                .not_null()
                .comment("状态-0:正常 1:禁用"),
        )
        .col(
            ColumnDef::new(App::CreatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("创建时间"),
        )
        .col(
            ColumnDef::new(App::UpdatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("更新时间"),
        )
        .to_owned()
}

fn create_app_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![Index::create()
        .name("idx_nw_app_plugin")
        .table(App::Table)
        .col(App::PluginId)
        .to_owned()])
}
