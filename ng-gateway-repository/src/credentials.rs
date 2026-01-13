use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    entities::prelude::{Credentials, CredentialsActiveModel, CredentialsColumn, CredentialsModel},
    enums::credentials::CredentialsType,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
};

/// Repository for device credentials operations
///
/// This repository manages the single set of device credentials for the gateway.
/// Only one record should exist in the database at any time representing the
/// gateway's current MQTT credentials.
pub struct CredentialsRepository;

impl CredentialsRepository {
    /// Create new credentials
    pub async fn create<C>(
        credentials: CredentialsActiveModel,
        db: Option<&C>,
    ) -> StorageResult<CredentialsModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(credentials.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(credentials.insert(&conn).await?)
            }
        }
    }

    /// Update existing credentials
    pub async fn update<C>(
        credentials: CredentialsActiveModel,
        db: Option<&C>,
    ) -> StorageResult<CredentialsModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(credentials.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(credentials.update(&conn).await?)
            }
        }
    }

    /// Delete credentials by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Credentials::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Credentials::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Get the current gateway credentials
    ///
    /// Since we only store one set of credentials for the gateway,
    /// this method returns the single credentials record if it exists.
    pub async fn get_current<C>(db: Option<&C>) -> StorageResult<Option<CredentialsModel>>
    where
        C: ConnectionTrait,
    {
        let credentials = match db {
            Some(conn) => Credentials::find().one(conn).await?,
            None => {
                let conn = get_db_connection().await?;
                Credentials::find().one(&conn).await?
            }
        };
        Ok(credentials)
    }

    /// Find credentials by ID
    pub async fn find_by_id<C>(id: i32, db: Option<&C>) -> StorageResult<Option<CredentialsModel>>
    where
        C: ConnectionTrait,
    {
        let credentials = match db {
            Some(conn) => Credentials::find_by_id(id).one(conn).await?,
            None => {
                let conn = get_db_connection().await?;
                Credentials::find_by_id(id).one(&conn).await?
            }
        };
        Ok(credentials)
    }

    /// Find credentials by type
    pub async fn find_by_type<C>(
        credentials_type: CredentialsType,
        db: Option<&C>,
    ) -> StorageResult<Vec<CredentialsModel>>
    where
        C: ConnectionTrait,
    {
        let credentials = match db {
            Some(conn) => {
                Credentials::find()
                    .filter(CredentialsColumn::Type.eq(credentials_type))
                    .all(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                Credentials::find()
                    .filter(CredentialsColumn::Type.eq(credentials_type))
                    .all(&conn)
                    .await?
            }
        };
        Ok(credentials)
    }

    /// Delete all existing credentials
    ///
    /// This is used when we need to replace credentials entirely.
    pub async fn delete_all<C>(db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Credentials::delete_many().exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Credentials::delete_many().exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Check if credentials exist
    pub async fn exists<C>(db: Option<&C>) -> StorageResult<bool>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let count = Credentials::find().count(conn).await?;
                Ok(count > 0)
            }
            None => {
                let conn = get_db_connection().await?;
                let count = Credentials::find().count(&conn).await?;
                Ok(count > 0)
            }
        }
    }
}
