use crate::{
    enums::common::{CollectionType, ReportType, Status},
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_CHANNEL_ORDER,
    create_table = create_channel_table,
    create_indexes = create_channel_indexes,
))]
pub enum Channel {
    Table,
    Id,
    DriverId,
    Name,
    CollectionType,
    Period,
    ReportType,
    Status,
    ConnectionPolicy,
    DriverConfig,
}

/// Create channel table
fn create_channel_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Channel::Table)
        .if_not_exists()
        .col(pk_auto(Channel::Id))
        .col(
            ColumnDef::new(Channel::DriverId)
                .integer()
                .not_null()
                .comment("Driver ID"),
        )
        .col(
            ColumnDef::new(Channel::Name)
                .string()
                .not_null()
                .comment("Channel name"),
        )
        .col(
            ColumnDef::new(Channel::CollectionType)
                .small_integer()
                .default(CollectionType::Collection)
                .not_null()
                .comment("Collection type"),
        )
        .col(
            ColumnDef::new(Channel::Period)
                .unsigned()
                .comment("Collection period in milliseconds"),
        )
        .col(
            ColumnDef::new(Channel::ReportType)
                .small_integer()
                .default(ReportType::Change)
                .not_null()
                .comment("Report type"),
        )
        .col(
            ColumnDef::new(Channel::Status)
                .small_integer()
                .default(Status::Enabled)
                .not_null()
                .comment("状态-0:正常 1:禁用"),
        )
        .col(
            ColumnDef::new(Channel::ConnectionPolicy)
                .json()
                .not_null()
                .comment("Connection policy JSON"),
        )
        .col(
            ColumnDef::new(Channel::DriverConfig)
                .json()
                .not_null()
                .comment("Driver configuration JSON"),
        )
        .to_owned()
}

/// Create channel indexes for SQLite
fn create_channel_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![Index::create()
        .name("idx_channel_driver_id")
        .table(Channel::Table)
        .col(Channel::DriverId)
        .to_owned()])
}
