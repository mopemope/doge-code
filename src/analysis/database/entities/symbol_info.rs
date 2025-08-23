//! Entity definitions for the repomap database.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "symbol_info")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(column_type = "Text")]
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub kind: String, // Store SymbolKind as text
    #[sea_orm(column_type = "Text")]
    pub file_path: String,
    pub start_line: i32,
    pub start_col: i32,
    pub end_line: i32,
    pub end_col: i32,
    #[sea_orm(column_type = "Text", nullable)]
    pub parent: Option<String>,
    pub file_total_lines: i32,
    #[sea_orm(column_type = "Integer", nullable)]
    pub function_lines: Option<i32>,
    #[sea_orm(column_type = "Text")]
    pub project_root: String, // Associate with project
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
