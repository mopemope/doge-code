use sea_orm::entity::prelude::*;

/// Rebuildable analysis metadata, committed with the corresponding RepoMap.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "repomap_metadata")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project_root: String,
    pub analysis_revision: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
