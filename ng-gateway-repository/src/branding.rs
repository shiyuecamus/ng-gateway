//! Repository for system branding.
//!
//! Branding is stored as a single row (id = 1). This repository provides
//! simple helpers for reading/updating that row.

use crate::get_db_connection;
use ng_gateway_error::{storage::StorageError, StorageResult};
use ng_gateway_models::entities::prelude::{
    Branding, BrandingActiveModel, BrandingColumn, BrandingModel,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};

/// Fixed primary key used for the system branding singleton row.
pub const BRANDING_SINGLETON_ID: i32 = 1;

pub struct BrandingRepository;

impl BrandingRepository {
    /// Load the branding singleton row.
    pub async fn get() -> StorageResult<Option<BrandingModel>> {
        let db = get_db_connection().await?;
        Ok(Branding::find_by_id(BRANDING_SINGLETON_ID).one(&db).await?)
    }

    /// Update branding title.
    pub async fn update_title(title: String) -> StorageResult<()> {
        let db = get_db_connection().await?;

        let mut active: BrandingActiveModel = Branding::find()
            .filter(BrandingColumn::Id.eq(BRANDING_SINGLETON_ID))
            .one(&db)
            .await?
            .ok_or_else(|| StorageError::EntityNotFound("branding".into()))?
            .into_active_model();

        active.app_title = Set(title);
        let _ = active.update(&db).await?;
        Ok(())
    }

    /// Update logo bytes and MIME.
    pub async fn update_logo(logo_mime: String, logo_bytes: Vec<u8>) -> StorageResult<()> {
        let db = get_db_connection().await?;

        let mut active: BrandingActiveModel = Branding::find()
            .filter(BrandingColumn::Id.eq(BRANDING_SINGLETON_ID))
            .one(&db)
            .await?
            .ok_or_else(|| StorageError::EntityNotFound("branding".into()))?
            .into_active_model();

        active.logo_mime = Set(logo_mime);
        active.logo_bytes = Set(logo_bytes);
        let _ = active.update(&db).await?;
        Ok(())
    }

    /// Update favicon bytes and MIME.
    pub async fn update_favicon(favicon_mime: String, favicon_bytes: Vec<u8>) -> StorageResult<()> {
        let db = get_db_connection().await?;

        let mut active: BrandingActiveModel = Branding::find()
            .filter(BrandingColumn::Id.eq(BRANDING_SINGLETON_ID))
            .one(&db)
            .await?
            .ok_or_else(|| StorageError::EntityNotFound("branding".into()))?
            .into_active_model();

        active.favicon_mime = Set(favicon_mime);
        active.favicon_bytes = Set(favicon_bytes);
        let _ = active.update(&db).await?;
        Ok(())
    }
}
