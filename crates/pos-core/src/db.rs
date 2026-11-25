use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// Create (or open) the local SQLite database and run migrations.
/// `path` may be a filesystem path or `":memory:"`.
pub async fn create_pool(path: &str) -> Result<SqlitePool, sqlx::Error> {
    let connect_string = if path == ":memory:" {
        // In-memory DBs are per-connection; keep a single connection so
        // the schema persists for the lifetime of the pool handle.
        "sqlite::memory:".to_string()
    } else {
        format!("sqlite:{path}")
    };
    let options = SqliteConnectOptions::from_str(&connect_string)?
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(if path == ":memory:" { 1 } else { 5 })
        .connect_with(options)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(include_str!("../migrations/0001_offline_init.sql"))
        .execute(pool)
        .await?;
    Ok(())
}
