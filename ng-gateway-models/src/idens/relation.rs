use super::{menu::Menu, role::Role, user::User};
use crate::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{NewMenuWithId, NewRelation, NewRoleWithId, NewUserWithId},
    enums::{common::EntityType, relation::RelationType},
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
};
use ng_gateway_macros::SeedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, SeedableInitializer)]
#[seedable(meta(
    model = NewRelation,
    order = super::INIT_RELATION_ORDER,
    create_table = create_relation_table,
    create_indexes = create_relation_indexes,
    seed_data = get_relation_seed_data
))]
pub enum Relation {
    Table,
    Id,
    FromId,
    FromType,
    ToId,
    ToType,
    Type,
    AdditionalInfo,
}

fn create_relation_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Relation::Table)
        .if_not_exists()
        .col(pk_auto(Relation::Id))
        .col(
            ColumnDef::new(Relation::FromId)
                .integer()
                .not_null()
                .comment("来源ID"),
        )
        .col(
            ColumnDef::new(Relation::FromType)
                .string_len(20)
                .not_null()
                .comment("来源类型"),
        )
        .col(
            ColumnDef::new(Relation::ToId)
                .integer()
                .not_null()
                .comment("目标ID"),
        )
        .col(
            ColumnDef::new(Relation::ToType)
                .string_len(20)
                .not_null()
                .comment("目标类型"),
        )
        .col(
            ColumnDef::new(Relation::Type)
                .string_len(20)
                .not_null()
                .comment("关系类型"),
        )
        .col(
            ColumnDef::new(Relation::AdditionalInfo)
                .json_binary()
                .comment("附加信息"),
        )
        .to_owned()
}

fn create_relation_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![
        Index::create()
            .name("idx_relation_from")
            .table(Relation::Table)
            .col(Relation::FromType)
            .col(Relation::FromId)
            .col(Relation::Type)
            .to_owned(),
        Index::create()
            .name("idx_relation_to")
            .table(Relation::Table)
            .col(Relation::ToType)
            .col(Relation::ToId)
            .col(Relation::Type)
            .to_owned(),
    ])
}

async fn get_relation_seed_data(ctx: &mut InitContext) -> Result<Option<Vec<NewRelation>>, DbErr> {
    let roles = ctx
        .get::<NewRoleWithId>(Role::Table.name())
        .map_err(|e| DbErr::Custom(format!("Failed to get role: {e}")))?;

    let system_admin_role = roles
        .iter()
        .find(|r| r.code == SYSTEM_ADMIN_ROLE_CODE)
        .unwrap();

    let users = ctx
        .get::<NewUserWithId>(User::Table.name())
        .map_err(|e| DbErr::Custom(format!("Failed to get user: {e}")))?;

    let menus = ctx
        .get::<NewMenuWithId>(Menu::Table.name())
        .map_err(|e| DbErr::Custom(format!("Failed to get menu: {e}")))?;

    let mut relations = Vec::new();

    // 添加用户-角色关系
    relations.extend(
        users
            .iter()
            .filter_map(|user| match user.username.as_str() {
                "system_admin" => Some(NewRelation {
                    from_id: user.id,
                    from_type: EntityType::User,
                    to_id: system_admin_role.id,
                    to_type: EntityType::Role,
                    r#type: RelationType::Contains,
                    additional_info: None,
                }),
                _ => None,
            }),
    );

    for menu in &menus {
        relations.push(NewRelation {
            from_id: system_admin_role.id,
            from_type: EntityType::Role,
            to_id: menu.id,
            to_type: EntityType::Menu,
            r#type: RelationType::Contains,
            additional_info: None,
        });
    }

    Ok(Some(relations))
}
