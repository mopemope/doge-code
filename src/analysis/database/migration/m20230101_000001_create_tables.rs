// Migration file for creating the initial schema for repomap persistence
use sea_orm_migration::prelude::*;

// Define table names
#[derive(DeriveIden)]
enum SymbolInfo {
    Table,
    Id,
    Name,
    Kind,
    FilePath,
    StartLine,
    StartCol,
    EndLine,
    EndCol,
    Parent,
    FileTotalLines,
    FunctionLines,
    ProjectRoot, // To associate symbols with a specific project
    CreatedAt,
}

#[derive(DeriveIden)]
enum FileHash {
    Table,
    Id,
    FilePath,
    Hash,
    ProjectRoot, // To associate hashes with a specific project
    CreatedAt,
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create the symbol_info table
        manager
            .create_table(
                Table::create()
                    .table(SymbolInfo::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SymbolInfo::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SymbolInfo::Name).text().not_null())
                    .col(ColumnDef::new(SymbolInfo::Kind).text().not_null()) // Store SymbolKind as text
                    .col(ColumnDef::new(SymbolInfo::FilePath).text().not_null())
                    .col(ColumnDef::new(SymbolInfo::StartLine).integer().not_null())
                    .col(ColumnDef::new(SymbolInfo::StartCol).integer().not_null())
                    .col(ColumnDef::new(SymbolInfo::EndLine).integer().not_null())
                    .col(ColumnDef::new(SymbolInfo::EndCol).integer().not_null())
                    .col(ColumnDef::new(SymbolInfo::Parent).text()) // Parent can be NULL
                    .col(
                        ColumnDef::new(SymbolInfo::FileTotalLines)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SymbolInfo::FunctionLines).integer()) // FunctionLines can be NULL
                    .col(ColumnDef::new(SymbolInfo::ProjectRoot).text().not_null()) // Associate with project
                    .col(
                        ColumnDef::new(SymbolInfo::CreatedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    // .index(
                    //     Index::create()
                    //         .name("idx_symbol_info_project_root")
                    //         .table(SymbolInfo::Table)
                    //         .col(SymbolInfo::ProjectRoot),
                    // )
                    // .index(
                    //     Index::create()
                    //         .name("idx_symbol_info_file_path")
                    //         .table(SymbolInfo::Table)
                    //         .col(SymbolInfo::FilePath),
                    // )
                    .to_owned(),
            )
            .await?;

        // Create the file_hash table
        manager
            .create_table(
                Table::create()
                    .table(FileHash::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FileHash::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FileHash::FilePath).text().not_null())
                    .col(ColumnDef::new(FileHash::Hash).text().not_null())
                    .col(ColumnDef::new(FileHash::ProjectRoot).text().not_null()) // Associate with project
                    .col(
                        ColumnDef::new(FileHash::CreatedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    // .index(
                    //     Index::create()
                    //         .name("idx_file_hash_project_root")
                    //         .table(FileHash::Table)
                    //         .col(FileHash::ProjectRoot),
                    // )
                    // .index(
                    //     Index::create()
                    //         .name("idx_file_hash_file_path")
                    //         .table(FileHash::Table)
                    //         .col(FileHash::FilePath),
                    // )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop the tables in reverse order
        manager
            .drop_table(Table::drop().table(FileHash::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SymbolInfo::Table).to_owned())
            .await?;

        Ok(())
    }
}
