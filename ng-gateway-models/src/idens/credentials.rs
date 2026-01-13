use crate::{
    enums::credentials::CredentialsType,
    initializer::{InitContext, NGInitializer},
};
use ng_gateway_macros::UnseedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, UnseedableInitializer)]
#[unseedable(meta(
    order = super::INIT_CREDENTIALS_ORDER,
    create_table = create_credentials_table
))]
pub enum Credentials {
    Table,
    Id,
    AccessToken,
    Type,
    Certificate,
    ClientId,
    Username,
    Password,
}

fn create_credentials_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Credentials::Table)
        .if_not_exists()
        .col(pk_auto(Credentials::Id))
        .col(
            ColumnDef::new(Credentials::Type)
                .small_integer()
                .default(CredentialsType::Basic)
                .not_null()
                .comment("证书类型"),
        )
        .col(
            ColumnDef::new(Credentials::AccessToken)
                .string_len(255)
                .not_null()
                .comment("访问令牌"),
        )
        .col(
            ColumnDef::new(Credentials::Certificate)
                .string()
                .comment("证书"),
        )
        .col(
            ColumnDef::new(Credentials::ClientId)
                .string_len(255)
                .comment("客户端ID"),
        )
        .col(
            ColumnDef::new(Credentials::Username)
                .string_len(255)
                .comment("用户名"),
        )
        .col(
            ColumnDef::new(Credentials::Password)
                .string_len(255)
                .comment("密码"),
        )
        .to_owned()
}
