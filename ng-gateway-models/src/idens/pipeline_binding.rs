use crate::{
    enums::common::Status,
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_PIPELINE_BINDING_ORDER,
    create_table = create_pipeline_binding_table,
    create_indexes = create_pipeline_binding_indexes,
))]
pub enum PipelineBinding {
    Table,
    Id,
    ChannelId,
    PipelineId,
    Status,
    CreatedAt,
    UpdatedAt,
}

/// Create AI pipeline binding table.
fn create_pipeline_binding_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(PipelineBinding::Table)
        .if_not_exists()
        .col(pk_auto(PipelineBinding::Id))
        .col(
            ColumnDef::new(PipelineBinding::ChannelId)
                .integer()
                .not_null()
                .comment("Channel ID"),
        )
        .col(
            ColumnDef::new(PipelineBinding::PipelineId)
                .integer()
                .not_null()
                .comment("Pipeline ID"),
        )
        .col(
            ColumnDef::new(PipelineBinding::Status)
                .small_integer()
                .not_null()
                .default(Status::Enabled)
                .comment("状态-0:启用 1:禁用"),
        )
        .col(
            ColumnDef::new(PipelineBinding::CreatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Pipeline Binding Created At"),
        )
        .col(
            ColumnDef::new(PipelineBinding::UpdatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Pipeline Binding Updated At"),
        )
        .to_owned()
}

/// Create AI pipeline binding indexes.
fn create_pipeline_binding_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("uk_pipeline_binding_channel")
            .table(PipelineBinding::Table)
            .col(PipelineBinding::ChannelId)
            .unique()
            .to_owned(),
        Index::create()
            .name("idx_pipeline_binding_pipeline")
            .table(PipelineBinding::Table)
            .col(PipelineBinding::PipelineId)
            .to_owned(),
    ])
}
