//! `SeaORM` Entity for the system branding table.
//!
//! This table stores **system-wide** branding configuration as a single row.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "branding")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Application title shown in UI and document title.
    pub app_title: String,
    /// Logo MIME type.
    pub logo_mime: String,
    /// Logo bytes stored as BLOB.
    pub logo_bytes: Vec<u8>,
    /// Favicon MIME type.
    pub favicon_mime: String,
    /// Favicon bytes stored as BLOB.
    pub favicon_bytes: Vec<u8>,
    pub created_at: Option<DateTimeUtc>,
    pub updated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
