use crate::initializer::{InitContext, NGInitializer};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_ACTION_ORDER,
    create_table = create_action_table,
    create_indexes = create_action_indexes,
))]
pub enum Action {
    Table,
    Id,
    DeviceId,
    Name,
    Command,
    Inputs,
}

/// Create action table
fn create_action_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Action::Table)
        .if_not_exists()
        .col(pk_auto(Action::Id))
        .col(
            ColumnDef::new(Action::DeviceId)
                .integer()
                .not_null()
                .comment("Device ID"),
        )
        .col(
            ColumnDef::new(Action::Name)
                .string()
                .not_null()
                .comment("Action name"),
        )
        .col(
            ColumnDef::new(Action::Command)
                .string()
                .not_null()
                .comment("Command to execute"),
        )
        .col(
            ColumnDef::new(Action::Inputs)
                .json()
                .not_null()
                .comment("Input parameters JSON"),
        )
        .to_owned()
}

/// Create action indexes for SQLite
fn create_action_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![Index::create()
        .name("idx_action_device_id")
        .table(Action::Table)
        .col(Action::DeviceId)
        .to_owned()])
}
