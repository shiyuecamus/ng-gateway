use crate::{
    domain::prelude::NewDriver,
    enums::common::{OsType, SourceType},
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
};
use ng_gateway_macros::SeedableInitializer;
use sea_orm::DatabaseBackend;
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[allow(clippy::enum_variant_names)]
#[derive(DeriveIden, SeedableInitializer)]
#[seedable(meta(
    model = NewDriver,
    order = super::INIT_DRIVER_ORDER,
    create_table = create_driver_table,
    create_indexes = create_driver_indexes,
    seed_data = get_driver_seed_data,
))]
pub enum Driver {
    Table,
    Id,
    Name,
    Description,
    DriverType,
    Source,
    Version,
    ApiVersion,
    SdkVersion,
    OsType,
    OsArch,
    Size,
    Path,
    Checksum,
    Metadata,
    CreatedAt,
    UpdatedAt,
}

fn create_driver_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Driver::Table)
        .if_not_exists()
        .col(pk_auto(Driver::Id))
        .col(
            ColumnDef::new(Driver::Name)
                .string_len(64)
                .not_null()
                .comment("Driver name"),
        )
        .col(
            ColumnDef::new(Driver::DriverType)
                .string_len(64)
                .not_null()
                .comment("Driver type"),
        )
        .col(
            ColumnDef::new(Driver::Description)
                .string_len(512)
                .comment("Driver description"),
        )
        .col(
            ColumnDef::new(Driver::Source)
                .small_integer()
                .default(SourceType::BuiltIn)
                .not_null()
                .comment("Driver source: 0 BuiltIn, 1 Custom"),
        )
        .col(
            ColumnDef::new(Driver::Version)
                .string_len(32)
                .not_null()
                .comment("Version"),
        )
        .col(
            ColumnDef::new(Driver::ApiVersion)
                .unsigned()
                .not_null()
                .comment("API version"),
        )
        .col(
            ColumnDef::new(Driver::SdkVersion)
                .string_len(32)
                .not_null()
                .comment("SDK version"),
        )
        .col(
            ColumnDef::new(Driver::OsType)
                .small_integer()
                .default(OsType::Linux)
                .not_null()
                .comment("OS type"),
        )
        .col(
            ColumnDef::new(Driver::OsArch)
                .small_integer()
                .not_null()
                .comment("OS arch"),
        )
        .col(
            ColumnDef::new(Driver::Size)
                .big_integer()
                .not_null()
                .comment("File size"),
        )
        .col(
            ColumnDef::new(Driver::Path)
                .string_len(512)
                .not_null()
                .comment("Local path"),
        )
        .col(
            ColumnDef::new(Driver::Checksum)
                .string_len(64)
                .not_null()
                .comment("Checksum"),
        )
        .col(
            ColumnDef::new(Driver::Metadata)
                .json_binary()
                .not_null()
                .comment("Metadata JSON"),
        )
        .col(
            ColumnDef::new(Driver::CreatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("创建时间"),
        )
        .col(
            ColumnDef::new(Driver::UpdatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("更新时间"),
        )
        .to_owned()
}

fn create_driver_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_driver_type")
            .table(Driver::Table)
            .col(Driver::DriverType)
            .to_owned(),
        Index::create()
            .name("idx_driver_arch")
            .table(Driver::Table)
            .col(Driver::OsType)
            .col(Driver::OsArch)
            .to_owned(),
    ])
}

async fn get_driver_seed_data(_: &mut InitContext) -> Result<Option<Vec<NewDriver>>, DbErr> {
    Ok(None)
}
