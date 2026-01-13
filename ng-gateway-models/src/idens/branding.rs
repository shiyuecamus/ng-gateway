//! Migration identifiers and seeding for system branding.
//!
//! Branding is inserted as a **single row** (id = 1) during database initialization.

use crate::{
    domain::prelude::NewBrandingWithId,
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
};
use ng_gateway_macros::SeedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

/// Default branding title when the system is first initialized.
const DEFAULT_APP_TITLE: &str = "NG Gateway";

/// Default favicon bytes embedded from the admin UI public assets.
///
/// # Note
/// This keeps defaults consistent between `pnpm dev:antd` and the gateway runtime.
const DEFAULT_FAVICON_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ng-gateway-ui/apps/web-antd/public/favicon.ico"
));

/// Default logo bytes embedded from the admin UI public assets.
const DEFAULT_LOGO_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ng-gateway-ui/apps/web-antd/public/static/logo.png"
));

#[derive(DeriveIden, SeedableInitializer)]
#[seedable(meta(
    model = NewBrandingWithId,
    order = super::INIT_SYSTEM_ORDER,
    create_table = create_branding_table,
    seed_data = get_branding_seed_data
))]
pub enum Branding {
    Table,
    Id,
    AppTitle,
    LogoMime,
    LogoBytes,
    FaviconMime,
    FaviconBytes,
    CreatedAt,
    UpdatedAt,
}

fn create_branding_table(_backend: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Branding::Table)
        .if_not_exists()
        .col(pk_auto(Branding::Id))
        .col(
            ColumnDef::new(Branding::AppTitle)
                .string_len(128)
                .not_null()
                .comment("Application title"),
        )
        .col(
            ColumnDef::new(Branding::LogoMime)
                .string_len(128)
                .not_null()
                .comment("Logo MIME type"),
        )
        .col(
            ColumnDef::new(Branding::LogoBytes)
                .binary()
                .not_null()
                .comment("Logo bytes (BLOB)"),
        )
        .col(
            ColumnDef::new(Branding::FaviconMime)
                .string_len(128)
                .not_null()
                .comment("Favicon MIME type"),
        )
        .col(
            ColumnDef::new(Branding::FaviconBytes)
                .binary()
                .not_null()
                .comment("Favicon bytes (BLOB)"),
        )
        .col(
            ColumnDef::new(Branding::CreatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("Created at"),
        )
        .col(
            ColumnDef::new(Branding::UpdatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("Updated at"),
        )
        .to_owned()
}

async fn get_branding_seed_data(
    _: &mut InitContext,
) -> Result<Option<Vec<NewBrandingWithId>>, DbErr> {
    Ok(Some(vec![NewBrandingWithId {
        id: 1,
        app_title: DEFAULT_APP_TITLE.into(),
        logo_mime: "image/png".into(),
        logo_bytes: DEFAULT_LOGO_BYTES.to_vec(),
        favicon_mime: "image/x-icon".into(),
        favicon_bytes: DEFAULT_FAVICON_BYTES.to_vec(),
    }]))
}
