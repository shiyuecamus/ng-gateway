use crate::{
    entities::casbin::{ActiveModel, Entity as CasbinEntity},
    initializer::SeedableTrait,
};
use sea_orm::{DeriveIntoActiveModel, IntoActiveModel};

#[derive(Clone, Debug, Default, PartialEq, DeriveIntoActiveModel)]
pub struct NewCasbin {
    pub ptype: Option<String>,
    pub v0: Option<String>,
    pub v1: Option<String>,
    pub v2: Option<String>,
    pub v3: Option<String>,
    pub v4: Option<String>,
    pub v5: Option<String>,
}

impl SeedableTrait for NewCasbin {
    type ActiveModel = ActiveModel;
    type Entity = CasbinEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel)]
pub struct UpdateCasbin {
    pub ptype: Option<Option<String>>,
    pub v0: Option<Option<String>>,
    pub v1: Option<Option<String>>,
    pub v2: Option<Option<String>>,
    pub v3: Option<Option<String>>,
    pub v4: Option<Option<String>>,
    pub v5: Option<Option<String>>,
}
