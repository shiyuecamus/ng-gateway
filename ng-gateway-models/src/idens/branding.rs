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
use std::path::{Path, PathBuf};

/// Default branding title when the system is first initialized.
const DEFAULT_APP_TITLE: &str = "NG Gateway";

/// Default logo MIME type for the seeded branding row.
const DEFAULT_LOGO_MIME: &str = "image/png";

/// Default favicon MIME type for the seeded branding row.
const DEFAULT_FAVICON_MIME: &str = "image/x-icon";

/// A minimal 1x1 transparent PNG.
///
/// Used as a fallback logo and also as the embedded image payload inside the
/// fallback favicon ICO.
const FALLBACK_PNG_1X1_TRANSPARENT: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 10, 73, 68, 65, 84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1, 13, 10,
    45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Builds a minimal ICO file containing the given PNG payload.
///
/// Many modern `.ico` files embed PNG data. The ICO structure is:
/// - 6 bytes header
/// - 16 bytes image directory entry (single image)
/// - PNG payload
fn build_ico_with_png(png: &[u8]) -> Vec<u8> {
    let png_len = <u32 as std::convert::TryFrom<usize>>::try_from(png.len()).unwrap_or_default();

    // ICONDIR
    let mut ico = Vec::with_capacity(6 + 16 + png.len());
    ico.extend_from_slice(&[0x00, 0x00]); // reserved
    ico.extend_from_slice(&[0x01, 0x00]); // type = icon
    ico.extend_from_slice(&[0x01, 0x00]); // count = 1

    // ICONDIRENTRY
    ico.push(0x01); // width = 1
    ico.push(0x01); // height = 1
    ico.push(0x00); // color count (0 = no palette)
    ico.push(0x00); // reserved
    ico.extend_from_slice(&[0x01, 0x00]); // planes
    ico.extend_from_slice(&[0x20, 0x00]); // bit count = 32
    ico.extend_from_slice(&png_len.to_le_bytes()); // size of image data
    ico.extend_from_slice(&u32::to_le_bytes(22)); // offset to image data

    // Payload
    ico.extend_from_slice(png);
    ico
}

/// Attempts to read an asset from multiple candidate paths.
///
/// # Behavior
/// - Returns the first successfully read file content.
/// - If all reads fail, returns the fallback bytes produced by `fallback`.
async fn read_asset_or_fallback<F>(name: &str, candidates: Vec<PathBuf>, fallback: F) -> Vec<u8>
where
    F: FnOnce() -> Vec<u8>,
{
    for path in candidates {
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                tracing::info!(
                    "[branding] loaded default asset name={} path={:?} size={}",
                    name,
                    path,
                    bytes.len()
                );
                return bytes;
            }
            Err(err) => {
                tracing::debug!(
                    "[branding] default asset read failed name={} path={:?} err={}",
                    name,
                    path,
                    err
                );
            }
        }
    }

    tracing::warn!(
        "[branding] default asset not found name={} (tried multiple paths); using fallback",
        name
    );
    fallback()
}

fn ui_asset_candidates(repo_relative_path: &str) -> Vec<PathBuf> {
    // Candidate #1: running from repo root (local dev / integration tests)
    let mut candidates = Vec::with_capacity(2);
    candidates.push(PathBuf::from(repo_relative_path));

    // Candidate #2: relative to the `ng-gateway-models` crate directory at build time.
    // This supports local builds where the UI submodule is checked out.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest_dir.join("..").join(repo_relative_path));

    candidates
}

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
    let favicon_bytes = read_asset_or_fallback(
        "favicon",
        ui_asset_candidates("ng-gateway-ui/apps/web-antd/public/favicon.ico"),
        || build_ico_with_png(FALLBACK_PNG_1X1_TRANSPARENT),
    )
    .await;

    let logo_bytes = read_asset_or_fallback(
        "logo",
        ui_asset_candidates("ng-gateway-ui/apps/web-antd/public/static/logo.png"),
        || FALLBACK_PNG_1X1_TRANSPARENT.to_vec(),
    )
    .await;

    Ok(Some(vec![NewBrandingWithId {
        id: 1,
        app_title: DEFAULT_APP_TITLE.into(),
        logo_mime: DEFAULT_LOGO_MIME.into(),
        logo_bytes,
        favicon_mime: DEFAULT_FAVICON_MIME.into(),
        favicon_bytes,
    }]))
}
