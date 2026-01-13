use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SymbolRelation::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SymbolRelation::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SymbolRelation::SourceSymbolId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SymbolRelation::TargetSymbolName)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SymbolRelation::RelationType)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SymbolRelation::Line).integer().not_null())
                    .col(
                        ColumnDef::new(SymbolRelation::CreatedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_symbol_relation_source_symbol_id")
                            .from(SymbolRelation::Table, SymbolRelation::SourceSymbolId)
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
            .drop_table(Table::drop().table(SymbolRelation::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub enum SymbolRelation {
    Table,
    Id,
    SourceSymbolId,
    TargetSymbolName,
    RelationType,
    Line,
    CreatedAt,
}

#[derive(DeriveIden)]
pub enum SymbolInfo {
    Table,
    Id,
}
