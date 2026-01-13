use crate::{
    domain::prelude::NewUserWithId,
    enums::common::Status,
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
};
use ng_gateway_macros::SeedableInitializer;
use ng_gateway_utils::hash;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, SeedableInitializer)]
#[seedable(meta(
    model = NewUserWithId,
    order = super::INIT_USER_ORDER,
    create_table = create_user_table,
    seed_data = get_user_seed_data
))]
pub enum User {
    Table,
    Id,
    Username,
    Password,
    Nickname,
    Avatar,
    Phone,
    Email,
    Status,
    AdditionalInfo,
    CreatedAt,
    UpdatedAt,
}

fn create_user_table(_backend: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(User::Table)
        .if_not_exists()
        .col(pk_auto(User::Id))
        .col(
            ColumnDef::new(User::Username)
                .string_len(128)
                .not_null()
                .comment("用户名"),
        )
        .col(
            ColumnDef::new(User::Password)
                .string_len(255)
                .not_null()
                .comment("密码"),
        )
        .col(
            ColumnDef::new(User::Nickname)
                .string_len(128)
                .not_null()
                .comment("昵称"),
        )
        .col(
            ColumnDef::new(User::Avatar)
                .string_len(255)
                .default("https://qmplusimg.henrongyi.top/gva_header.jpg")
                .comment("头像"),
        )
        .col(
            ColumnDef::new(User::Phone)
                .string_len(128)
                .comment("手机号"),
        )
        .col(ColumnDef::new(User::Email).string_len(255).comment("邮箱"))
        .col(
            ColumnDef::new(User::Status)
                .small_integer()
                .default(Status::Enabled)
                .not_null()
                .comment("状态-0:正常 1:禁用"),
        )
        .col(
            ColumnDef::new(User::AdditionalInfo)
                .json_binary()
                .comment("附加信息"),
        )
        .col(
            ColumnDef::new(User::CreatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("创建时间"),
        )
        .col(
            ColumnDef::new(User::UpdatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("更新时间"),
        )
        .to_owned()
}

async fn get_user_seed_data(_: &mut InitContext) -> Result<Option<Vec<NewUserWithId>>, DbErr> {
    Ok(Some(vec![NewUserWithId {
        id: 1,
        username: "system_admin".into(),
        password: hash::bcrypt_hash("system_admin"),
        nickname: "System Administrator".into(),
        avatar: Some("https://qmplusimg.henrongyi.top/gva_header.jpg".into()),
        phone: Some("17600000000".into()),
        email: Some("system_admin@example.com".into()),
        ..Default::default()
    }]))
}
