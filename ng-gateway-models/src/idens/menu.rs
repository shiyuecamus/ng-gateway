use crate::{
    domain::prelude::NewMenuWithId,
    enums::{common::Status, menu::MenuType},
    initializer::{
        DataSeederTrait, InitContext, NGInitializer, SeedableInitializerTrait, SeedableTrait,
    },
    DEFAULT_ROOT_TREE_ID,
};
use ng_gateway_macros::SeedableInitializer;
use sea_orm::{DatabaseBackend, DeriveIden};
use sea_orm_migration::{prelude::*, schema::pk_auto};

#[derive(DeriveIden, SeedableInitializer)]
#[seedable(meta(
    model = NewMenuWithId,
    order = super::INIT_MENU_ORDER,
    create_table = create_menu_table,
    create_indexes = create_menu_indexes,
    seed_data = get_menu_seed_data,
))]
#[allow(clippy::enum_variant_names)]
pub enum Menu {
    Table,
    Id,
    Name,
    ParentId,
    Type,
    Path,
    Component,
    Sort,
    Title,
    Icon,
    AffixTab,
    KeepAlive,

    HideInMenu,
    HideChildrenInMenu,
    Status,
    CreatedAt,
    UpdatedAt,
}
fn create_menu_table(_: DatabaseBackend) -> TableCreateStatement {
    Table::create()
        .table(Menu::Table)
        .if_not_exists()
        .col(pk_auto(Menu::Id))
        .col(
            ColumnDef::new(Menu::Name)
                .string_len(128)
                .not_null()
                .comment("名称"),
        )
        .col(
            ColumnDef::new(Menu::ParentId)
                .integer()
                .not_null()
                .comment("父菜单ID"),
        )
        .col(
            ColumnDef::new(Menu::Type)
                .small_integer()
                .not_null()
                .default(MenuType::Menu)
                .comment("菜单类型-0:目录 1:菜单"),
        )
        .col(
            ColumnDef::new(Menu::Path)
                .string_len(255)
                .not_null()
                .comment("路由地址"),
        )
        .col(
            ColumnDef::new(Menu::Component)
                .string_len(255)
                .not_null()
                .comment("组件路径"),
        )
        .col(
            ColumnDef::new(Menu::Sort)
                .integer()
                .default(0)
                .comment("排序"),
        )
        .col(
            ColumnDef::new(Menu::Title)
                .string_len(255)
                .not_null()
                .comment("标题"),
        )
        .col(ColumnDef::new(Menu::Icon).string_len(255).comment("图标"))
        .col(
            ColumnDef::new(Menu::AffixTab)
                .boolean()
                .default(false)
                .not_null()
                .comment("是否固定标签"),
        )
        .col(
            ColumnDef::new(Menu::KeepAlive)
                .boolean()
                .default(false)
                .not_null()
                .comment("是否缓存"),
        )
        .col(
            ColumnDef::new(Menu::HideInMenu)
                .boolean()
                .default(false)
                .not_null()
                .comment("是否隐藏"),
        )
        .col(
            ColumnDef::new(Menu::HideChildrenInMenu)
                .boolean()
                .default(false)
                .not_null()
                .comment("是否隐藏子菜单"),
        )
        .col(
            ColumnDef::new(Menu::Status)
                .small_integer()
                .not_null()
                .default(Status::Enabled)
                .comment("状态-0:启用 1:禁用"),
        )
        .col(
            ColumnDef::new(Menu::CreatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("创建时间"),
        )
        .col(
            ColumnDef::new(Menu::UpdatedAt)
                .timestamp()
                .default(Expr::current_timestamp())
                .comment("更新时间"),
        )
        .to_owned()
}

fn create_menu_indexes(_: DatabaseBackend) -> Option<Vec<IndexCreateStatement>> {
    Some(vec![Index::create()
        .name("idx_menu_parent")
        .table(Menu::Table)
        .col(Menu::ParentId)
        .col(Menu::Sort)
        .to_owned()])
}

async fn get_menu_seed_data(_: &mut InitContext) -> Result<Option<Vec<NewMenuWithId>>, DbErr> {
    Ok(Some(vec![
        NewMenuWithId {
            id: 1,
            name: "首页".into(),
            parent_id: DEFAULT_ROOT_TREE_ID,
            r#type: MenuType::Menu,
            path: Some("/home".into()),
            component: "/dashboard/analytics/index".into(),
            sort: Some(1),
            title: "page.home.title".into(),
            icon: Some("ic:baseline-home".into()),
            affix_tab: true,
            keep_alive: true,
            ..Default::default()
        },
        NewMenuWithId {
            id: 2,
            name: "系统".into(),
            parent_id: DEFAULT_ROOT_TREE_ID,
            r#type: MenuType::Directory,
            path: Some("/system".into()),
            component: "BasicLayout".into(),
            sort: Some(2),
            title: "page.system.title".into(),
            icon: Some("mdi:cogs".into()),
            ..Default::default()
        },
        NewMenuWithId {
            id: 3,
            name: "用户".into(),
            parent_id: 2,
            r#type: MenuType::Menu,
            path: Some("/system/user".into()),
            component: "/system/user/index".into(),
            sort: Some(4),
            title: "page.system.user.title".into(),
            icon: Some("ic:outline-person".into()),
            keep_alive: true,
            ..Default::default()
        },
        // NewMenuWithId {
        //     id: 4,
        //     name: "角色".into(),
        //     parent_id: 2,
        //     r#type: MenuType::Menu,
        //     path: Some("/system/role".into()),
        //     component: "/system/role/index".into(),
        //     sort: Some(5),
        //     title: "page.system.role.title".into(),
        //     icon: Some("mdi:account-circle-outline".into()),
        //     keep_alive: true,
        //     ..Default::default()
        // },
        // NewMenuWithId {
        //     id: 5,
        //     name: "菜单".into(),
        //     parent_id: 2,
        //     r#type: MenuType::Menu,
        //     path: Some("/system/menu".into()),
        //     component: "/system/menu/index".into(),
        //     sort: Some(6),
        //     title: "page.system.menu.title".into(),
        //     icon: Some("mdi:view-list-outline".into()),
        //     keep_alive: true,
        //     ..Default::default()
        // },
        NewMenuWithId {
            id: 6,
            name: "北向".into(),
            parent_id: DEFAULT_ROOT_TREE_ID,
            r#type: MenuType::Directory,
            path: Some("/northward".into()),
            component: "BasicLayout".into(),
            sort: Some(7),
            title: "page.northward.title".into(),
            icon: Some("mdi:cloud-upload".into()),
            ..Default::default()
        },
        NewMenuWithId {
            id: 7,
            name: "插件".into(),
            parent_id: 6,
            r#type: MenuType::Menu,
            path: Some("/northward/plugin".into()),
            component: "/northward/plugin/index".into(),
            sort: Some(1),
            title: "page.northward.plugin.title".into(),
            icon: Some("mdi:puzzle".into()),
            keep_alive: true,
            ..Default::default()
        },
        NewMenuWithId {
            id: 8,
            name: "应用".into(),
            parent_id: 6,
            r#type: MenuType::Menu,
            path: Some("/northward/app".into()),
            component: "/northward/app/index".into(),
            sort: Some(2),
            title: "page.northward.app.title".into(),
            icon: Some("mdi:application".into()),
            keep_alive: true,
            ..Default::default()
        },
        NewMenuWithId {
            id: 9,
            name: "南向".into(),
            parent_id: DEFAULT_ROOT_TREE_ID,
            r#type: MenuType::Menu,
            path: Some("/southward".into()),
            component: "BasicLayout".into(),
            sort: Some(7),
            title: "page.southward.title".into(),
            icon: Some("mdi:chip".into()),
            ..Default::default()
        },
        NewMenuWithId {
            id: 10,
            name: "通道".into(),
            parent_id: 9,
            r#type: MenuType::Menu,
            path: Some("/southward/channel".into()),
            component: "/southward/channel/index".into(),
            sort: Some(2),
            title: "page.southward.channel.title".into(),
            icon: Some("mdi:radio-tower".into()),
            keep_alive: true,
            ..Default::default()
        },
        NewMenuWithId {
            id: 11,
            name: "驱动".into(),
            parent_id: 9,
            r#type: MenuType::Menu,
            path: Some("/southward/driver".into()),
            component: "/southward/driver/index".into(),
            sort: Some(1),
            title: "page.southward.driver.title".into(),
            icon: Some("mdi:power-plug".into()),
            keep_alive: true,
            ..Default::default()
        },
        NewMenuWithId {
            id: 12,
            name: "运维".into(),
            parent_id: DEFAULT_ROOT_TREE_ID,
            r#type: MenuType::Menu,
            path: Some("/maintenance".into()),
            component: "BasicLayout".into(),
            sort: Some(7),
            title: "page.maintenance.title".into(),
            icon: Some("mdi:wrench".into()),
            ..Default::default()
        },
        NewMenuWithId {
            id: 13,
            name: "监控".into(),
            parent_id: 12,
            r#type: MenuType::Menu,
            path: Some("/maintenance/monitor".into()),
            component: "/maintenance/monitor/index".into(),
            sort: Some(2),
            title: "page.maintenance.monitor.title".into(),
            icon: Some("mdi:monitor".into()),
            keep_alive: true,
            ..Default::default()
        },
        NewMenuWithId {
            id: 14,
            name: "调试".into(),
            parent_id: 12,
            r#type: MenuType::Menu,
            path: Some("/maintenance/debug".into()),
            component: "/maintenance/debug/index".into(),
            sort: Some(3),
            title: "page.maintenance.debug.title".into(),
            icon: Some("mdi:bug".into()),
            keep_alive: true,
            ..Default::default()
        },
        NewMenuWithId {
            id: 15,
            name: "品牌".into(),
            parent_id: 12,
            r#type: MenuType::Menu,
            path: Some("/maintenance/branding".into()),
            component: "/maintenance/branding/index".into(),
            sort: Some(4),
            title: "page.maintenance.branding.title".into(),
            icon: Some("mdi:palette".into()),
            keep_alive: true,
            ..Default::default()
        },
    ]))
}
