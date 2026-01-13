use ng_gateway_error::NGResult;
use ng_gateway_models::settings::{NGDbConfig, Sqlite};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use std::time::Duration;
use tracing::{info, instrument, log::LevelFilter};

#[instrument(name = "init_sqlite_db", skip_all)]
/// Initialize SQLite database connection with auto-creation support
/// Uses SQLite URL parameter mode=rwc for automatic file creation when auto_create is enabled
pub async fn init_db(config: &Sqlite) -> NGResult<DatabaseConnection> {
    let database_url = config.to_url();

    let mut opts = ConnectOptions::new(&database_url);
    opts.connect_timeout(Duration::from_millis(config.timeout))
        .idle_timeout(Duration::from_millis(config.idle_timeout))
        .max_lifetime(Duration::from_millis(config.max_lifetime))
        .max_connections(config.max_connections);

    #[cfg(debug_assertions)]
    {
        opts.sqlx_logging(true)
            .sqlx_logging_level(LevelFilter::Info);
    }
    #[cfg(not(debug_assertions))]
    {
        opts.sqlx_logging(false)
            .sqlx_logging_level(LevelFilter::Off);
    }

    info!(
        "Connecting to SQLite database at: {} (auto_create: {})",
        config.db_path(),
        config.auto_create
    );

    let db = Database::connect(opts).await?;
    // Performance-oriented PRAGMA settings (without WAL) for release builds.
    // WAL is intentionally disabled to remain safe on NFS / network filesystems.
    #[cfg(not(debug_assertions))]
    {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let _ = db
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA synchronous=NORMAL;".to_string(),
            ))
            .await;
        let _ = db
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA temp_store=MEMORY;".to_string(),
            ))
            .await;
        let _ = db
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA cache_size=-20000;".to_string(),
            ))
            .await;
    }
    info!("Successfully connected to SQLite database");

    Ok(db)
}
