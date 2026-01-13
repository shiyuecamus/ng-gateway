use crate::{
    entities::menu::{ActiveModel, Entity as MenuEntity},
    enums::{common::Status, menu::MenuType},
    initializer::SeedableTrait,
};
use ng_gateway_utils::tree::TreeNode;
use sea_orm::{
    DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct NewMenu {
    pub name: String,
    pub parent_id: i32,
    pub r#type: MenuType,
    pub path: String,
    pub component: String,
    pub sort: Option<i32>,
    pub title: String,
    pub icon: Option<String>,
    pub affix_tab: bool,
    pub keep_alive: bool,
    pub hide_in_menu: bool,
    pub hide_children_in_menu: bool,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct NewMenuWithId {
    pub id: i32,
    pub name: String,
    pub parent_id: i32,
    pub r#type: MenuType,
    pub path: Option<String>,
    pub component: String,
    pub sort: Option<i32>,
    pub title: String,
    pub icon: Option<String>,
    pub affix_tab: bool,
    pub keep_alive: bool,
    pub hide_in_menu: bool,
    pub hide_children_in_menu: bool,
}

impl SeedableTrait for NewMenuWithId {
    type ActiveModel = ActiveModel;
    type Entity = MenuEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct UpdateMenu {
    pub id: i32,
    pub name: String,
    pub parent_id: i32,
    pub r#type: MenuType,
    pub path: String,
    pub component: String,
    pub sort: Option<Option<i32>>,
    pub title: String,
    pub icon: Option<Option<String>>,
    pub affix_tab: bool,
    pub keep_alive: bool,
    pub hide_in_menu: bool,
    pub hide_children_in_menu: bool,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct ChangeMenuStatus {
    pub id: i32,
    pub status: Status,
}

/// Menu metadata structure
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuMeta {
    /// Sorting order value
    #[serde(rename = "order")]
    pub sort: Option<i32>,
    /// Menu title
    pub title: String,
    /// Whether to keep as a fixed tab
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affix_tab: Option<bool>,
    /// Whether to keep the component alive
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<bool>,
    /// Menu icon
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Whether to skip using the basic layout
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_basic_layout: Option<bool>,
    /// Whether to hide this item in menu
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_in_menu: Option<bool>,
    /// Whether to hide children in menu
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_children_in_menu: Option<bool>,
}

/// Menu tree structure
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuTree {
    /// Menu ID
    pub id: i32,
    /// Parent menu ID
    pub parent_id: i32,
    /// Menu name
    pub name: String,
    /// Menu path
    pub path: String,
    /// Component path
    pub component: Option<String>,
    /// Menu metadata
    pub meta: MenuMeta,
    /// Child menus
    pub children: Vec<MenuTree>,
    /// Whether this node has children
    pub has_children: bool,
}

/// Information about menu model to implement BuildTree trait
#[derive(Debug, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::MenuModel as ModelTrait>::Entity")]
pub struct MenuInfo {
    /// Menu ID
    pub id: i32,
    /// Parent menu ID
    pub parent_id: i32,
    /// Menu name
    pub name: String,
    /// Menu type
    pub r#type: MenuType,
    /// Menu path
    pub path: Option<String>,
    /// Component path
    pub component: Option<String>,
    /// Sort value
    pub sort: Option<i32>,
    /// Menu title
    pub title: Option<String>,
    /// Menu icon
    pub icon: Option<String>,
    /// Whether to keep as a fixed tab
    pub affix_tab: bool,
    /// Whether to keep component alive
    pub keep_alive: bool,
    /// Whether to hide in menu
    pub hide_in_menu: bool,
    /// Whether to hide children in menu
    pub hide_children_in_menu: bool,
}

impl TreeNode for MenuTree {
    type Id = i32;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn parent_id(&self) -> Self::Id {
        self.parent_id
    }

    fn children(&self) -> &[Self] {
        &self.children
    }

    fn children_mut(&mut self) -> &mut Vec<Self> {
        &mut self.children
    }

    fn has_children(&self) -> bool {
        !self.children.is_empty()
    }

    fn has_children_mut(&mut self) -> &mut bool {
        &mut self.has_children
    }

    fn sort_key(&self) -> i32 {
        self.meta.sort.unwrap_or(0)
    }
}

impl From<MenuInfo> for MenuTree {
    fn from(info: MenuInfo) -> Self {
        MenuTree {
            id: info.id,
            parent_id: info.parent_id,
            name: info.name,
            path: info.path.clone().unwrap_or_default(),
            component: info.component.clone(),
            meta: MenuMeta {
                sort: info.sort,
                title: info.title.clone().unwrap_or_default(),
                affix_tab: if info.affix_tab { Some(true) } else { None },
                keep_alive: if info.keep_alive { Some(true) } else { None },
                icon: info.icon.clone(),
                no_basic_layout: None,
                hide_in_menu: if info.hide_in_menu { Some(true) } else { None },
                hide_children_in_menu: if info.hide_children_in_menu {
                    Some(true)
                } else {
                    None
                },
            },
            children: Vec::new(),
            has_children: false,
        }
    }
}
