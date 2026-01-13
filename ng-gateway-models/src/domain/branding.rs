//! System branding domain models.
//!
//! This module defines **system-wide** branding configuration:
//! - Application title
//! - Logo bytes + MIME type
//! - Favicon bytes + MIME type
//!
//! # Design
//! Branding is persisted as a **single row** in SQLite (id = 1 recommended).
//! Binary assets are stored as BLOBs to simplify deployment and avoid filesystem coupling.

use crate::{
    entities::branding::{ActiveModel, Entity as BrandingEntity, Model as BrandingModel},
    initializer::SeedableTrait,
};
use chrono::{DateTime, Utc};
use sea_orm::{DeriveIntoActiveModel, IntoActiveModel};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Public branding configuration payload for unauthenticated clients.
///
/// This is used by the frontend during bootstrap and by the login page
/// to render title/logo/favicon before the user is authenticated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandingPublicConfig {
    /// Application title to be used for document title.
    pub title: String,
    /// Absolute (or root-relative) URL to fetch the current logo.
    pub logo_url: String,
    /// Absolute (or root-relative) URL to fetch the current favicon.
    pub favicon_url: String,
    /// Updated timestamp used for cache busting and ETag generation.
    pub updated_at: Option<DateTime<Utc>>,
}

impl BrandingPublicConfig {
    /// Convert DB model to a public payload.
    #[inline]
    pub fn from_model(model: &BrandingModel, logo_url: String, favicon_url: String) -> Self {
        Self {
            title: model.app_title.clone(),
            logo_url,
            favicon_url,
            updated_at: model.updated_at,
        }
    }
}

/// Update payload for changing the application title.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBrandingTitle {
    /// New title to apply.
    #[validate(length(min = 1, max = 128, message = "title length must be 1..=128"))]
    pub title: String,
}

/// Seed model for inserting the initial branding row.
///
/// # Notes
/// This struct is used by the database initializer for seeding default data.
#[derive(Clone, Debug, Default, PartialEq, DeriveIntoActiveModel)]
pub struct NewBrandingWithId {
    /// Primary key.
    pub id: i32,
    /// Default application title.
    pub app_title: String,
    /// Logo MIME type, e.g. `image/png`.
    pub logo_mime: String,
    /// Logo bytes.
    pub logo_bytes: Vec<u8>,
    /// Favicon MIME type, e.g. `image/x-icon`.
    pub favicon_mime: String,
    /// Favicon bytes.
    pub favicon_bytes: Vec<u8>,
}

impl SeedableTrait for NewBrandingWithId {
    type ActiveModel = ActiveModel;
    type Entity = BrandingEntity;

    #[inline]
    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}
