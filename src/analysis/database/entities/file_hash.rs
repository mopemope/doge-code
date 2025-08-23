//! Entity definitions for the repomap database.
use sea_orm::entity::prelude::*;

/// FileHash entity.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "file_hash")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(column_type = "Text")]
    pub file_path: String,
    #[sea_orm(column_type = "Text")]
    pub hash: String,
    #[sea_orm(column_type = "Text")]
    pub project_root: String, // Associate with project
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
