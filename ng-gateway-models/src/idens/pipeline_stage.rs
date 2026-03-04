use crate::initializer::{InitContext, NGInitializer};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_PIPELINE_STAGE_ORDER,
    create_table = create_pipeline_stage_table,
    create_indexes = create_pipeline_stage_indexes,
))]
pub enum PipelineStage {
    Table,
    Id,
    PipelineId,
    StageOrder,
    Config,
    CreatedAt,
    UpdatedAt,
}

/// Create AI pipeline stage table.
fn create_pipeline_stage_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(PipelineStage::Table)
        .if_not_exists()
        .col(pk_auto(PipelineStage::Id))
        .col(
            ColumnDef::new(PipelineStage::PipelineId)
                .integer()
                .not_null()
                .comment("FK: ai_pipeline.id"),
        )
        .col(
            ColumnDef::new(PipelineStage::StageOrder)
                .integer()
                .not_null()
                .comment("Pipeline stage order"),
        )
        .col(
            ColumnDef::new(PipelineStage::Config)
                .json()
                .not_null()
                .comment("Pipeline stage config JSON"),
        )
        .col(
            ColumnDef::new(PipelineStage::CreatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Created at timestamp"),
        )
        .col(
            ColumnDef::new(PipelineStage::UpdatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Updated at timestamp"),
        )
        .to_owned()
}

/// Create AI pipeline stage indexes.
fn create_pipeline_stage_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_pipeline_stage_pipeline")
            .table(PipelineStage::Table)
            .col(PipelineStage::PipelineId)
            .to_owned(),
        Index::create()
            .name("uk_pipeline_stage_order")
            .table(PipelineStage::Table)
            .col(PipelineStage::PipelineId)
            .col(PipelineStage::StageOrder)
            .unique()
            .to_owned(),
    ])
}
