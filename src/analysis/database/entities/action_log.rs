use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "action_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(indexed)]
    pub session_id: String,
    pub timestamp: String,   // ISO8601 string
    pub action_type: String, // "tool_use", "user_msg", "verification_result"
    #[sea_orm(column_type = "Text")]
    pub content: String, // Prompt, Code, or Tool Output
    #[sea_orm(column_type = "Json")]
    pub error: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
