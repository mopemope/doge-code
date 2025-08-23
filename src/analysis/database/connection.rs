use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};
use std::time::Duration;
use tracing::info;

/// Connects to the SQLite database.
///
/// # Arguments
/// * `db_path` - Path to the SQLite database file.
///
/// # Returns
/// * `Result<DatabaseConnection, DbErr>` - The database connection or an error.
pub async fn connect_database(db_path: &str) -> Result<DatabaseConnection, DbErr> {
    info!("Connecting to SQLite database at: {}", db_path);

    let mut opt = ConnectOptions::new(format!("sqlite://{}?mode=rwc", db_path)); // mode=rwc: read, write, create
    opt.max_connections(10)
        .min_connections(5)
        .connect_timeout(Duration::from_secs(8))
        .acquire_timeout(Duration::from_secs(8))
        .idle_timeout(Duration::from_secs(8))
        .max_lifetime(Duration::from_secs(8))
        .sqlx_logging(true) // Enable SQLx logging if needed
        .sqlx_logging_level(tracing::log::LevelFilter::Info); // Set logging level

    Database::connect(opt).await
}

/// Gets the default database path for repomap storage.
///
/// # Arguments
/// * `project_root` - The root path of the project.
///
/// # Returns
/// * `String` - The path to the SQLite database file.
pub fn get_default_db_path(project_root: &std::path::Path) -> String {
    project_root
        .join(".doge")
        .join("repomap.sqlite")
        .to_string_lossy()
        .to_string()
}
