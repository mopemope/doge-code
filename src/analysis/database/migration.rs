//! Migration module for repomap database.
pub mod m20230101_000001_create_tables;

use crate::analysis::database::migration::m20230101_000001_create_tables::Migration as CreateTablesMigration;
use sea_orm::{DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// Migrator for repomap database.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![Box::new(CreateTablesMigration)]
    }
}

/// Runs all pending migrations.
///
/// # Arguments
/// * `db_conn` - The database connection.
///
/// # Returns
/// * `Result<(), DbErr>` - Ok if successful, Err otherwise.
pub async fn run_migrations(db_conn: &DatabaseConnection) -> Result<(), DbErr> {
    Migrator::up(db_conn, None).await
}
