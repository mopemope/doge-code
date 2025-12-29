use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ActionLog::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ActionLog::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ActionLog::SessionId).string().not_null())
                    .col(ColumnDef::new(ActionLog::Timestamp).string().not_null())
                    .col(ColumnDef::new(ActionLog::ActionType).string().not_null())
                    .col(ColumnDef::new(ActionLog::Content).text().not_null())
                    .col(ColumnDef::new(ActionLog::Metadata).json().not_null())
                    .col(ColumnDef::new(ActionLog::Embedding).blob().not_null())
                    .to_owned(),
            )
            .await?;

        // Create index for session_id
        manager
            .create_index(
                Index::create()
                    .name("idx_action_log_session_id")
                    .table(ActionLog::Table)
                    .col(ActionLog::SessionId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ActionLog::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum ActionLog {
    Table,
    Id,
    SessionId,
    Timestamp,
    ActionType,
    Content,
    Metadata,
    Embedding,
}
