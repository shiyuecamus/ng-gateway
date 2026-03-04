use crate::{
    enums::common::Status,
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_PIPELINE_ORDER,
    create_table = create_pipeline_table,
    create_indexes = create_pipeline_indexes,
))]
pub enum Pipeline {
    Table,
    Id,
    Key,
    Name,
    Sampling,
    RoiRegions,
    Annotation,
    Status,
    Revision,
    CreatedAt,
    UpdatedAt,
}

/// Create AI pipeline table.
fn create_pipeline_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Pipeline::Table)
        .if_not_exists()
        .col(pk_auto(Pipeline::Id))
        .col(
            ColumnDef::new(Pipeline::Key)
                .string()
                .not_null()
                .comment("Pipeline key"),
        )
        .col(
            ColumnDef::new(Pipeline::Name)
                .string()
                .not_null()
                .comment("Pipeline name"),
        )
        .col(
            ColumnDef::new(Pipeline::Sampling)
                .json()
                .not_null()
                .comment("Pipeline sampling"),
        )
        .col(
            ColumnDef::new(Pipeline::RoiRegions)
                .json()
                .not_null()
                .default(serde_json::json!([]))
                .comment("Pipeline ROI regions"),
        )
        .col(
            ColumnDef::new(Pipeline::Annotation)
                .json()
                .not_null()
                .comment("Pipeline annotation"),
        )
        .col(
            ColumnDef::new(Pipeline::Status)
                .small_integer()
                .not_null()
                .default(Status::Enabled)
                .comment("状态-0:启用 1:禁用"),
        )
        .col(
            ColumnDef::new(Pipeline::Revision)
                .unsigned()
                .not_null()
                .default(1)
                .comment("Pipeline revision"),
        )
        .col(
            ColumnDef::new(Pipeline::CreatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Created at timestamp"),
        )
        .col(
            ColumnDef::new(Pipeline::UpdatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Updated at timestamp"),
        )
        .to_owned()
}

/// Create AI pipeline indexes.
fn create_pipeline_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("uk_pipeline_pipeline_key")
            .table(Pipeline::Table)
            .col(Pipeline::Key)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_pipeline_status")
            .table(Pipeline::Table)
            .col(Pipeline::Status)
            .to_owned(),
    ])
}
