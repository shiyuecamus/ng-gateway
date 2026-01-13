use crate::{
    entities::relation::{ActiveModel, Entity as RelationEntity},
    enums::{common::EntityType, relation::RelationType},
    initializer::SeedableTrait,
};
use sea_orm::{
    DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::RelationModel as ModelTrait>::Entity")]
pub struct RelationInfo {
    pub from_id: i32,
    pub from_type: EntityType,
    pub to_id: i32,
    pub to_type: EntityType,
    pub r#type: RelationType,
    pub additional_info: Option<Json>,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct NewRelation {
    pub from_id: i32,
    pub from_type: EntityType,
    pub to_id: i32,
    pub to_type: EntityType,
    pub r#type: RelationType,
    pub additional_info: Option<Json>,
}

impl SeedableTrait for NewRelation {
    type ActiveModel = ActiveModel;
    type Entity = RelationEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct UpdateRelation {
    pub from_id: i32,
    pub from_type: EntityType,
    pub to_id: i32,
    pub to_type: EntityType,
    pub r#type: RelationType,
    pub additional_info: Option<Option<Json>>,
}

pub struct RelationQuery {
    pub from_id: Option<i32>,
    pub from_type: Option<EntityType>,
    pub to_id: Option<i32>,
    pub to_type: Option<EntityType>,
}

pub struct RelationDelete {
    pub from_id: Option<i32>,
    pub from_type: Option<EntityType>,
    pub to_id: Option<i32>,
    pub to_type: Option<EntityType>,
    pub r#type: Option<RelationType>,
}
