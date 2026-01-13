use crate::{
    enums::common::Status,
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[allow(clippy::enum_variant_names)]
#[unseedable(meta(
    order = super::INIT_DEVICE_ORDER,
    create_table = create_device_table,
    create_indexes = create_device_indexes,
))]
pub enum Device {
    Table,
    Id,
    ChannelId,
    DeviceName,
    DeviceType,
    Status,
    DriverConfig,
}

/// Create device table
fn create_device_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Device::Table)
        .if_not_exists()
        .col(pk_auto(Device::Id))
        .col(
            ColumnDef::new(Device::ChannelId)
                .integer()
                .not_null()
                .comment("Channel ID"),
        )
        .col(
            ColumnDef::new(Device::DeviceName)
                .string()
                .not_null()
                .comment("Device name"),
        )
        .col(
            ColumnDef::new(Device::DeviceType)
                .string()
                .not_null()
                .comment("Device type"),
        )
        .col(
            ColumnDef::new(Device::Status)
                .small_integer()
                .default(Status::Enabled)
                .not_null()
                .comment("状态-0:正常 1:禁用"),
        )
        .col(
            ColumnDef::new(Device::DriverConfig)
                .json()
                .comment("Driver configuration JSON"),
        )
        .to_owned()
}

/// Create device indexes for SQLite
fn create_device_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_device_channel_id")
            .table(Device::Table)
            .col(Device::ChannelId)
            .to_owned(),
        Index::create()
            .name("ux_device_channel_name")
            .table(Device::Table)
            .col(Device::ChannelId)
            .col(Device::DeviceName)
            .unique()
            .to_owned(),
    ])
}
