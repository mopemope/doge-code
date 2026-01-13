//! Entity definitions for the symbol_relation table.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "symbol_relation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub source_symbol_id: i32,
    #[sea_orm(column_type = "Text")]
    pub target_symbol_name: String,
    #[sea_orm(column_type = "Text")]
    pub relation_type: String,
    pub line: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::symbol_info::Entity",
        from = "Column::SourceSymbolId",
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
