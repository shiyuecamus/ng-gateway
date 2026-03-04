use crate::initializer::{InitContext, NGInitializer};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_ALARM_RULE_ORDER,
    create_table = create_alarm_rule_table,
    create_indexes = create_alarm_rule_indexes,
))]
pub enum AlarmRule {
    Table,
    Id,
    PipelineId,
    RuleOrder,
    Name,
    Severity,
    Condition,
    CooldownSecs,
    MinDurationSecs,
    CreatedAt,
    UpdatedAt,
}

/// Create AI alarm rule table.
fn create_alarm_rule_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(AlarmRule::Table)
        .if_not_exists()
        .col(pk_auto(AlarmRule::Id))
        .col(
            ColumnDef::new(AlarmRule::PipelineId)
                .integer()
                .not_null()
                .comment("Pipeline ID"),
        )
        .col(
            ColumnDef::new(AlarmRule::RuleOrder)
                .integer()
                .not_null()
                .comment("Rule Order"),
        )
        .col(
            ColumnDef::new(AlarmRule::Name)
                .string()
                .not_null()
                .comment("Rule Name"),
        )
        .col(
            ColumnDef::new(AlarmRule::Severity)
                .string()
                .not_null()
                .comment("Rule Severity"),
        )
        .col(
            ColumnDef::new(AlarmRule::Condition)
                .json()
                .not_null()
                .comment("Rule Condition"),
        )
        .col(
            ColumnDef::new(AlarmRule::CooldownSecs)
                .unsigned()
                .not_null()
                .comment("Rule Cooldown Seconds"),
        )
        .col(
            ColumnDef::new(AlarmRule::MinDurationSecs)
                .unsigned()
                .comment("Rule Minimum Duration Seconds"),
        )
        .col(
            ColumnDef::new(AlarmRule::CreatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Rule Created At"),
        )
        .col(
            ColumnDef::new(AlarmRule::UpdatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Rule Updated At"),
        )
        .to_owned()
}

/// Create AI alarm rule indexes.
fn create_alarm_rule_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_alarm_rule_pipeline")
            .table(AlarmRule::Table)
            .col(AlarmRule::PipelineId)
            .to_owned(),
        Index::create()
            .name("uk_alarm_rule_order")
            .table(AlarmRule::Table)
            .col(AlarmRule::PipelineId)
            .col(AlarmRule::RuleOrder)
            .unique()
            .to_owned(),
    ])
}
