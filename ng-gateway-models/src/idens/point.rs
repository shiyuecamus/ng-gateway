use crate::{
    enums::common::{AccessMode, DataPointType, DataType},
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_POINT_ORDER,
    create_table = create_point_table,
    create_indexes = create_point_indexes,
))]
pub enum Point {
    Table,
    Id,
    DeviceId,
    Name,
    Key,
    #[sea_orm(column_name = "type")]
    Type,
    DataType,
    AccessMode,
    Unit,
    MinValue,
    MaxValue,
    Scale,
    DriverConfig,
}

/// Create point table
fn create_point_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Point::Table)
        .if_not_exists()
        .col(pk_auto(Point::Id))
        .col(
            ColumnDef::new(Point::DeviceId)
                .integer()
                .not_null()
                .comment("Channel ID"),
        )
        .col(
            ColumnDef::new(Point::Name)
                .string()
                .not_null()
                .comment("Point name"),
        )
        .col(
            ColumnDef::new(Point::Key)
                .string()
                .not_null()
                .comment("Point key"),
        )
        .col(
            ColumnDef::new(Point::Type)
                .small_integer()
                .default(DataPointType::Telemetry)
                .not_null()
                .comment("Point type"),
        )
        .col(
            ColumnDef::new(Point::DataType)
                .small_integer()
                .default(DataType::Int32)
                .not_null()
                .comment("Data type"),
        )
        .col(
            ColumnDef::new(Point::AccessMode)
                .small_integer()
                .default(AccessMode::Read)
                .not_null()
                .comment("Access mode"),
        )
        .col(
            ColumnDef::new(Point::Unit)
                .string()
                .comment("Unit of measurement"),
        )
        .col(
            ColumnDef::new(Point::MinValue)
                .double()
                .comment("Minimum value"),
        )
        .col(
            ColumnDef::new(Point::MaxValue)
                .double()
                .comment("Maximum value"),
        )
        .col(
            ColumnDef::new(Point::Scale)
                .double()
                .comment("Scale factor"),
        )
        .col(
            ColumnDef::new(Point::DriverConfig)
                .json()
                .not_null()
                .comment("Driver configuration JSON"),
        )
        .to_owned()
}

/// Create point indexes for SQLite
fn create_point_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_point_device_id")
            .table(Point::Table)
            .col(Point::DeviceId)
            .to_owned(),
        Index::create()
            .name("ux_point_device_name")
            .table(Point::Table)
            .col(Point::DeviceId)
            .col(Point::Name)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_point_device_access")
            .table(Point::Table)
            .col(Point::DeviceId)
            .col(Point::AccessMode)
            .to_owned(),
    ])
}
