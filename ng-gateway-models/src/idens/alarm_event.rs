use crate::initializer::{InitContext, NGInitializer};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_ALARM_EVENT_ORDER,
    create_table = create_alarm_event_table,
    create_indexes = create_alarm_event_indexes,
))]
pub enum AlarmEvent {
    Table,
    Id,
    ChannelId,
    PipelineId,
    AlarmType,
    Severity,
    Description,
    Payload,
    Status,
    AckedAt,
    ClosedAt,
    CreatedAt,
    UpdatedAt,
}

/// Create AI alarm event table.
fn create_alarm_event_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(AlarmEvent::Table)
        .if_not_exists()
        .col(pk_auto(AlarmEvent::Id))
        .col(
            ColumnDef::new(AlarmEvent::ChannelId)
                .integer()
                .not_null()
                .comment("Channel ID"),
        )
        .col(
            ColumnDef::new(AlarmEvent::PipelineId)
                .integer()
                .comment("Pipeline ID"),
        )
        .col(
            ColumnDef::new(AlarmEvent::AlarmType)
                .string()
                .not_null()
                .comment("Alarm Type"),
        )
        .col(
            ColumnDef::new(AlarmEvent::Severity)
                .string()
                .not_null()
                .comment("Alarm Severity"),
        )
        .col(
            ColumnDef::new(AlarmEvent::Description)
                .string()
                .not_null()
                .default("")
                .comment("Alarm Description"),
        )
        .col(
            ColumnDef::new(AlarmEvent::Payload)
                .json()
                .comment("Alarm Payload"),
        )
        .col(
            ColumnDef::new(AlarmEvent::Status)
                .string()
                .not_null()
                .default("open")
                .comment("Alarm Status"),
        )
        .col(
            ColumnDef::new(AlarmEvent::AckedAt)
                .timestamp()
                .comment("Alarm Acked At"),
        )
        .col(
            ColumnDef::new(AlarmEvent::ClosedAt)
                .timestamp()
                .comment("Alarm Closed At"),
        )
        .col(
            ColumnDef::new(AlarmEvent::CreatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Alarm Created At"),
        )
        .col(
            ColumnDef::new(AlarmEvent::UpdatedAt)
                .timestamp()
                .not_null()
                .default(Expr::current_timestamp())
                .comment("Alarm Updated At"),
        )
        .to_owned()
}

/// Create AI alarm event indexes.
fn create_alarm_event_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_alarm_event_channel")
            .table(AlarmEvent::Table)
            .col(AlarmEvent::ChannelId)
            .to_owned(),
        Index::create()
            .name("idx_alarm_event_pipeline")
            .table(AlarmEvent::Table)
            .col(AlarmEvent::PipelineId)
            .to_owned(),
        Index::create()
            .name("idx_alarm_event_status_created")
            .table(AlarmEvent::Table)
            .col(AlarmEvent::Status)
            .col(AlarmEvent::CreatedAt)
            .to_owned(),
    ])
}
