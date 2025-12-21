use sea_orm_migration::prelude::*;

#[derive(DeriveIden)]
enum SymbolEmbedding {
    Table,
    Id,
    SymbolId,
    Embedding,
    ModelVersion,
    CreatedAt,
}

#[derive(DeriveIden)]
enum SymbolInfo {
    Table,
    Id,
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SymbolEmbedding::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SymbolEmbedding::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SymbolEmbedding::SymbolId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SymbolEmbedding::Embedding)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SymbolEmbedding::ModelVersion)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SymbolEmbedding::CreatedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_symbol_embedding_symbol_id")
                            .from(SymbolEmbedding::Table, SymbolEmbedding::SymbolId)
                            .to(SymbolInfo::Table, SymbolInfo::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SymbolEmbedding::Table).to_owned())
            .await
    }
}
