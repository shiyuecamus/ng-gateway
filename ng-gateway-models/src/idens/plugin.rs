use crate::{
    domain::prelude::NewPlugin,
    enums::common::{OsArch, OsType, SourceType},
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
};
use ng_gateway_macros::SeedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, SeedableInitializer)]
#[allow(clippy::enum_variant_names)]
#[seedable(meta(
    model = NewPlugin,
    order = super::INIT_PLUGIN_ORDER,
    create_table = create_plugin_table,
    create_indexes = create_plugin_indexes,
    seed_data = get_plugin_seed_data,
))]
pub enum Plugin {
    Table,
    Id,
    Name,
    Description,
    PluginType,
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

fn create_plugin_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Plugin::Table)
        .if_not_exists()
        .col(pk_auto(Plugin::Id))
        .col(
            ColumnDef::new(Plugin::Name)
                .string_len(64)
                .not_null()
                .comment("Plugin name"),
        )
        .col(
            ColumnDef::new(Plugin::PluginType)
                .string_len(64)
                .not_null()
                .comment("Plugin type identifier"),
        )
        .col(
            ColumnDef::new(Plugin::Description)
                .string_len(512)
                .comment("Plugin description"),
        )
        .col(
            ColumnDef::new(Plugin::Source)
                .small_integer()
                .default(SourceType::BuiltIn)
                .not_null()
                .comment("Plugin source: 0 BuiltIn, 1 Custom"),
        )
        .col(
            ColumnDef::new(Plugin::Version)
                .string_len(32)
                .not_null()
                .comment("Version"),
        )
        .col(
            ColumnDef::new(Plugin::ApiVersion)
                .unsigned()
                .not_null()
                .comment("API version"),
        )
        .col(
            ColumnDef::new(Plugin::SdkVersion)
                .string_len(32)
                .not_null()
                .comment("SDK version"),
        )
        .col(
            ColumnDef::new(Plugin::OsType)
                .small_integer()
                .default(OsType::Unknown)
                .not_null()
                .comment("OS type"),
        )
        .col(
            ColumnDef::new(Plugin::OsArch)
                .small_integer()
                .default(OsArch::Unknown)
                .not_null()
                .comment("OS arch"),
        )
        .col(
            ColumnDef::new(Plugin::Size)
                .big_integer()
                .not_null()
                .comment("File size (0 for built-in)"),
        )
        .col(
            ColumnDef::new(Plugin::Path)
                .string_len(512)
                .not_null()
                .comment("Library path or builtin scheme"),
        )
        .col(
            ColumnDef::new(Plugin::Checksum)
                .string_len(64)
                .not_null()
                .comment("Checksum (empty for built-in)"),
        )
        .col(
            ColumnDef::new(Plugin::Metadata)
                .json_binary()
                .not_null()
                .comment("Metadata JSON"),
        )
        .col(
            ColumnDef::new(Plugin::CreatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("创建时间"),
        )
        .col(
            ColumnDef::new(Plugin::UpdatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("更新时间"),
        )
        .to_owned()
}

fn create_plugin_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_nw_plugin_type")
            .table(Plugin::Table)
            .col(Plugin::PluginType)
            .to_owned(),
        Index::create()
            .name("idx_nw_plugin_arch")
            .table(Plugin::Table)
            .col(Plugin::OsType)
            .col(Plugin::OsArch)
            .to_owned(),
        Index::create()
            .name("ux_nw_plugin_name_version")
            .table(Plugin::Table)
            .col(Plugin::Name)
            .col(Plugin::Version)
            .unique()
            .to_owned(),
    ])
}

async fn get_plugin_seed_data(_: &mut InitContext) -> Result<Option<Vec<NewPlugin>>, DbErr> {
    Ok(None)
}
