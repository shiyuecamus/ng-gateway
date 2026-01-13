use crate::initializer::{InitContext, NGInitializer};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

/// Northward app extension table for plugin-specific persistent data
///
/// This table stores key-value pairs for each northward app instance,
/// allowing plugins to persist configuration, credentials, and other state data.
///
/// # Design
/// - Each row represents one key-value pair for an app
/// - JSON values provide maximum flexibility
/// - No created_at/updated_at (not meaningful for extensions)
/// - Foreign key constraint ensures data cleanup when app is deleted
#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_APP_EXT_ORDER,
    create_table = create_app_ext_table,
    create_indexes = create_app_ext_indexes,
))]
pub enum AppExt {
    Table,
    Id,
    /// Foreign key to northward_app.id
    AppId,
    /// Extension key (unique per app)
    Key,
    /// Extension value (JSON)
    Value,
}

fn create_app_ext_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(AppExt::Table)
        .if_not_exists()
        .col(pk_auto(AppExt::Id))
        .col(
            ColumnDef::new(AppExt::AppId)
                .integer()
                .not_null()
                .comment("FK: northward_app.id"),
        )
        .col(
            ColumnDef::new(AppExt::Key)
                .string_len(255)
                .not_null()
                .comment("Extension key"),
        )
        .col(
            ColumnDef::new(AppExt::Value)
                .json_binary()
                .not_null()
                .comment("Extension value (JSON)"),
        )
        .to_owned()
}

fn create_app_ext_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        // Unique constraint on (app_id, key)
        Index::create()
            .name("uk_nw_app_ext_app_key")
            .table(AppExt::Table)
            .col(AppExt::AppId)
            .col(AppExt::Key)
            .unique()
            .to_owned(),
        // Index on app_id for efficient queries
        Index::create()
            .name("idx_nw_app_ext_app_id")
            .table(AppExt::Table)
            .col(AppExt::AppId)
            .to_owned(),
    ])
}
