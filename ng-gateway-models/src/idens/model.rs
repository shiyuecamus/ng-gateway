use crate::{
    enums::common::Status,
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_MODEL_ORDER,
    create_table = create_model_table,
    create_indexes = create_model_indexes,
))]
pub enum Model {
    Table,
    Id,
    Key,
    Name,
    Version,
    Task,
    Format,
    Path,
    Labels,
    DefaultPreprocess,
    DefaultPostprocess,
    Inputs,
    Outputs,
    Size,
    Status,
    Checksum,
    CreatedAt,
    UpdatedAt,
}

/// Create AI model metadata table.
fn create_model_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Model::Table)
        .if_not_exists()
        .col(pk_auto(Model::Id))
        .col(
            ColumnDef::new(Model::Key)
                .string()
                .not_null()
                .comment("Model key"),
        )
        .col(
            ColumnDef::new(Model::Name)
                .string()
                .not_null()
                .comment("Model name"),
        )
        .col(
            ColumnDef::new(Model::Version)
                .string()
                .not_null()
                .comment("Model version"),
        )
        .col(
            ColumnDef::new(Model::Task)
                .string()
                .not_null()
                .comment("Model task"),
        )
        .col(
            ColumnDef::new(Model::Format)
                .string()
                .not_null()
                .comment("Model format"),
        )
        .col(
            ColumnDef::new(Model::Path)
                .string()
                .not_null()
                .comment("Model path"),
        )
        .col(ColumnDef::new(Model::Labels).json().comment("Model labels"))
        .col(
            ColumnDef::new(Model::DefaultPreprocess)
                .json()
                .comment("Model default preprocess"),
        )
        .col(
            ColumnDef::new(Model::DefaultPostprocess)
                .json()
                .comment("Model default postprocess"),
        )
        .col(ColumnDef::new(Model::Inputs).json().comment("Model inputs"))
        .col(
            ColumnDef::new(Model::Outputs)
                .json()
                .comment("Model outputs"),
        )
        .col(
            ColumnDef::new(Model::Size)
                .big_unsigned()
                .not_null()
                .comment("Model size"),
        )
        .col(
            ColumnDef::new(Model::Status)
                .small_integer()
                .not_null()
                .default(Status::Enabled)
                .comment("状态-0:启用 1:禁用"),
        )
        .col(
            ColumnDef::new(Model::Checksum)
                .string()
                .not_null()
                .comment("Model checksum"),
        )
        .col(
            ColumnDef::new(Model::CreatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Created at timestamp"),
        )
        .col(
            ColumnDef::new(Model::UpdatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Updated at timestamp"),
        )
        .to_owned()
}

/// Create AI model indexes.
fn create_model_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("uk_ai_model_key")
            .table(Model::Table)
            .col(Model::Key)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_model_status")
            .table(Model::Table)
            .col(Model::Status)
            .to_owned(),
    ])
}
