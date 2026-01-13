use ng_gateway_models::initializer::{initializers, InitContext};
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};
use tracing::{info, instrument};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        create_tables(manager).await?;
        create_indexes(manager).await?;
        create_sqlite_updated_at_triggers(manager).await?;
        seeding_data(manager).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for initializer in initializers() {
            manager
                .drop_table(initializer.to_drop_table_stmt(manager.get_database_backend()))
                .await?;
        }
        Ok(())
    }
}

async fn create_tables(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    for initializer in initializers() {
        manager
            .create_table(initializer.to_create_table_stmt(backend))
            .await?;
    }
    Ok(())
}

async fn create_indexes(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for initializer in initializers() {
        for stmt in initializer
            .to_create_indexes_stmt(manager.get_database_backend())
            .unwrap_or_default()
        {
            manager.create_index(stmt).await?;
        }
    }
    Ok(())
}

/// Create SQLite triggers to automatically update the `updated_at` column on row updates.
///
/// For SQLite, column defaults do not support `ON UPDATE CURRENT_TIMESTAMP`. We therefore
/// create an `AFTER UPDATE` trigger per table that contains an `UpdatedAt` column. The
/// trigger updates the `updated_at` field only when the application has not explicitly
/// changed it, and it uses a `WHEN` clause to prevent infinite recursion.
async fn create_sqlite_updated_at_triggers(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    if manager.get_database_backend() != DatabaseBackend::Sqlite {
        return Ok(());
    }

    let conn = manager.get_connection();
    for initializer in initializers() {
        if !initializer.has_update_col() {
            continue;
        }

        let table_name = initializer.name();
        let trigger_name = format!("trg_{}_updated_at", table_name);
        let sql = format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS "{trigger_name}"
            AFTER UPDATE ON "{table_name}"
            FOR EACH ROW
            WHEN NEW."updated_at" = OLD."updated_at"
            BEGIN
                UPDATE "{table_name}" SET "updated_at" = CURRENT_TIMESTAMP WHERE rowid = NEW.rowid;
            END;
            "#
        );

        conn.execute(Statement::from_string(DatabaseBackend::Sqlite, sql))
            .await?;
    }

    Ok(())
}

#[instrument(name = "seeding-data", skip_all)]
async fn seeding_data(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let mut ctx = InitContext::default();
    for initializer in initializers() {
        let transaction = db.begin().await?;
        info!(initializer = initializer.name(), "start seeding data",);
        initializer.seeding_data(&transaction, &mut ctx).await?;
        transaction.commit().await?;
        info!(initializer = initializer.name(), "seeding data success",);
    }
    Ok(())
}
