// Migration file for adding keywords column to symbol_info table
use sea_orm_migration::prelude::*;

#[derive(DeriveIden)]
enum SymbolInfo {
    Table,
    Keywords, // New column for storing keywords
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Add keywords column to symbol_info table
        manager
            .alter_table(
                Table::alter()
                    .table(SymbolInfo::Table)
                    .add_column(ColumnDef::new(SymbolInfo::Keywords).text().default(""))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Remove keywords column from symbol_info table
        manager
            .alter_table(
                Table::alter()
                    .table(SymbolInfo::Table)
                    .drop_column(SymbolInfo::Keywords)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}
