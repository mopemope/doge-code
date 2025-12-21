//! Entity definitions for the symbol_embedding table.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "symbol_embedding")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub symbol_id: i32,
    #[sea_orm(column_type = "Blob")]
    pub embedding: Vec<u8>,
    #[sea_orm(column_type = "Text")]
    pub model_version: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::symbol_info::Entity",
        from = "Column::SymbolId",
        to = "super::symbol_info::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    SymbolInfo,
}

impl Related<super::symbol_info::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SymbolInfo.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
