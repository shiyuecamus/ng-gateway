use crate::{
    enums::common::Status,
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_ALGORITHM_ORDER,
    create_table = create_algorithm_table,
    create_indexes = create_algorithm_indexes,
))]
pub enum Algorithm {
    Table,
    Id,
    Key,
    Name,
    Description,
    Version,
    ModuleType,
    Path,
    ConfigSchema,
    Size,
    Status,
    Checksum,
    CreatedAt,
    UpdatedAt,
}

/// Create AI algorithm metadata table.
fn create_algorithm_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Algorithm::Table)
        .if_not_exists()
        .col(pk_auto(Algorithm::Id))
        .col(
            ColumnDef::new(Algorithm::Key)
                .string()
                .not_null()
                .comment("Algorithm key"),
        )
        .col(ColumnDef::new(Algorithm::Name).string().not_null())
        .col(
            ColumnDef::new(Algorithm::Description)
                .string()
                .comment("Algorithm description"),
        )
        .col(
            ColumnDef::new(Algorithm::Version)
                .string()
                .not_null()
                .comment("Algorithm version"),
        )
        .col(
            ColumnDef::new(Algorithm::ModuleType)
                .string()
                .not_null()
                .comment("Algorithm module type"),
        )
        .col(
            ColumnDef::new(Algorithm::Path)
                .string()
                .not_null()
                .comment("Algorithm path"),
        )
        .col(
            ColumnDef::new(Algorithm::ConfigSchema)
                .json()
                .comment("Algorithm config schema"),
        )
        .col(
            ColumnDef::new(Algorithm::Size)
                .big_unsigned()
                .not_null()
                .comment("Algorithm size"),
        )
        .col(
            ColumnDef::new(Algorithm::Status)
                .small_integer()
                .not_null()
                .default(Status::Enabled)
                .comment("状态-0:启用 1:禁用"),
        )
        .col(
            ColumnDef::new(Algorithm::Checksum)
                .string()
                .not_null()
                .comment("Algorithm checksum"),
        )
        .col(
            ColumnDef::new(Algorithm::CreatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Created at timestamp"),
        )
        .col(
            ColumnDef::new(Algorithm::UpdatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Updated at timestamp"),
        )
        .to_owned()
}

/// Create AI algorithm indexes.
fn create_algorithm_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("uk_algorithm_key")
            .table(Algorithm::Table)
            .col(Algorithm::Key)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_algorithm_status")
            .table(Algorithm::Table)
            .col(Algorithm::Status)
            .to_owned(),
    ])
}
