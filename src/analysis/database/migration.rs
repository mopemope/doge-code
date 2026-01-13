//! Migration module for repomap database.
pub mod m20230101_000001_create_tables;
pub mod m20230101_000002_add_keywords_to_symbol_info;

pub mod m20251230_000001_create_action_log_table;

use crate::analysis::database::migration::m20230101_000001_create_tables::Migration as CreateTablesMigration;
use crate::analysis::database::migration::m20230101_000002_add_keywords_to_symbol_info::Migration as AddKeywordsMigration;

use crate::analysis::database::migration::m20251230_000001_create_action_log_table::Migration as CreateActionLogTableMigration;
use sea_orm::{DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// Migrator for repomap database.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![
            Box::new(CreateTablesMigration),
            Box::new(AddKeywordsMigration),
            Box::new(CreateActionLogTableMigration),
        ]
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
