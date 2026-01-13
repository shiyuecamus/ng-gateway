use crate::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{NewCasbin, NewRoleWithId, NewUserWithId},
    enums::common::EntityType,
    idens::role::Role,
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
};
use ng_gateway_macros::SeedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

use super::user::User;

#[derive(DeriveIden, SeedableInitializer)]
#[seedable(meta(
    model = NewCasbin,
    order = super::INIT_CASBIN_ORDER,
    create_table = create_casbin_rule_table,
    seed_data = get_casbin_rule_seed_data
))]
pub enum Casbin {
    Table,
    Id,
    Ptype,
    V0,
    V1,
    V2,
    V3,
    V4,
    V5,
}

fn create_casbin_rule_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Casbin::Table)
        .if_not_exists()
        .col(pk_auto(Casbin::Id))
        .col(ColumnDef::new(Casbin::Ptype).string())
        .col(ColumnDef::new(Casbin::V0).string())
        .col(ColumnDef::new(Casbin::V1).string())
        .col(ColumnDef::new(Casbin::V2).string())
        .col(ColumnDef::new(Casbin::V3).string())
        .col(ColumnDef::new(Casbin::V4).string())
        .col(ColumnDef::new(Casbin::V5).string())
        .to_owned()
}

async fn get_casbin_rule_seed_data(ctx: &mut InitContext) -> Result<Option<Vec<NewCasbin>>, DbErr> {
    let users = ctx
        .get::<NewUserWithId>(User::Table.name())
        .map_err(|e| DbErr::Custom(format!("Failed to get user: {e}")))?;

    let roles = ctx
        .get::<NewRoleWithId>(Role::Table.name())
        .map_err(|e| DbErr::Custom(format!("Failed to get role: {e}")))?;

    let system_admin_role = roles
        .iter()
        .find(|r| r.code == SYSTEM_ADMIN_ROLE_CODE)
        .unwrap();

    let mut rules = users
        .iter()
        .map(|user| NewCasbin {
            ptype: Some("g".to_string()),
            v0: Some(user.username.clone()),
            v1: Some(SYSTEM_ADMIN_ROLE_CODE.to_string()),
            ..Default::default()
        })
        .collect::<Vec<_>>();

    for entity_type in EntityType::all() {
        let entity_name = entity_type.to_string();
        for operation in entity_type.operations() {
            rules.push(NewCasbin {
                ptype: Some("p".to_string()),
                v0: Some(system_admin_role.code.clone()),
                v1: Some(entity_name.clone()),
                v2: Some(operation.to_string()),
                v3: Some("resource".to_string()),
                ..Default::default()
            });
        }
    }
    Ok(Some(rules))
}
