use crate::{
    constants::{SYSTEM_ADMIN_ROLE_CODE, SYSTEM_ADMIN_ROLE_NAME},
    domain::prelude::NewRoleWithId,
    enums::{common::Status, role::RoleType},
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
};
use ng_gateway_macros::SeedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, SeedableInitializer)]
#[seedable(meta(
    model = NewRoleWithId,
    order = super::INIT_ROLE_ORDER,
    create_table = create_role_table,
    seed_data = get_role_seed_data,
))]
pub enum Role {
    Table,
    Id,
    Name,
    Code,
    Sort,
    Type,
    Status,
    AdditionalInfo,
    CreatedAt,
    UpdatedAt,
}

fn create_role_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Role::Table)
        .if_not_exists()
        .col(pk_auto(Role::Id))
        .col(
            ColumnDef::new(Role::Name)
                .string_len(128)
                .not_null()
                .comment("名称"),
        )
        .col(
            ColumnDef::new(Role::Code)
                .string_len(50)
                .not_null()
                .comment("编码"),
        )
        .col(
            ColumnDef::new(Role::Sort)
                .integer()
                .default(0)
                .comment("排序"),
        )
        .col(
            ColumnDef::new(Role::Type)
                .small_integer()
                .default(RoleType::Custom)
                .not_null()
                .comment("类型"),
        )
        .col(
            ColumnDef::new(Role::Status)
                .small_integer()
                .default(Status::Enabled)
                .not_null()
                .comment("状态-0:正常 1:禁用"),
        )
        .col(
            ColumnDef::new(Role::AdditionalInfo)
                .json_binary()
                .comment("附加信息"),
        )
        .col(
            ColumnDef::new(Role::CreatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("创建时间"),
        )
        .col(
            ColumnDef::new(Role::UpdatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("更新时间"),
        )
        .to_owned()
}

async fn get_role_seed_data(_: &mut InitContext) -> Result<Option<Vec<NewRoleWithId>>, DbErr> {
    let roles = vec![NewRoleWithId {
        id: 1,
        name: SYSTEM_ADMIN_ROLE_NAME.into(),
        code: SYSTEM_ADMIN_ROLE_CODE.into(),
        sort: Some(1),
        r#type: RoleType::BuiltIn,
        additional_info: None,
    }];
    Ok(Some(roles))
}
