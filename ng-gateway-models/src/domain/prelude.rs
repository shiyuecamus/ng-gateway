use serde::{Deserialize, Serialize};
use validator::Validate;

pub use crate::domain::{
    action::{ActionInfo, ActionPageParams, NewAction, UpdateAction},
    app::{AppInfo, AppPageParams, ChangeAppStatus, NewApp, UpdateApp},
    app_sub::{
        AppSubInfo, AppSubPageParams, ChannelDeviceTree, DeviceTreeNode, NewAppSub, UpdateAppSub,
    },
    auth::{Claims, LoginRequest, LoginResponse},
    branding::{BrandingPublicConfig, NewBrandingWithId, UpdateBrandingTitle},
    casbin::{NewCasbin, UpdateCasbin},
    channel::{ChangeChannelStatus, ChannelInfo, ChannelPageParams, NewChannel, UpdateChannel},
    common::{
        BatchDeletePayload, ClearByChannelPayload, ClearByDevicePayload, PageParams, PageResult,
    },
    credentials::{NewCredentials, UpdateCredentials},
    device::{ChangeDeviceStatus, DeviceInfo, DevicePageParams, NewDevice, UpdateDevice},
    driver::{
        CommitResult, DriverInfo, DriverPageParams, ImportPreview, NewDriver, PathEntityId,
        TemplateQuery, UpdateDriver,
    },
    menu::{ChangeMenuStatus, MenuInfo, MenuMeta, MenuTree, NewMenu, NewMenuWithId, UpdateMenu},
    plugin::{NewPlugin, PluginInfo, PluginPageParams, UpdatePlugin},
    point::{NewPoint, PointInfo, PointPageParams, UpdatePoint},
    relation::{NewRelation, RelationDelete, RelationInfo, RelationQuery, UpdateRelation},
    role::{
        ChangeRoleStatus, NewRole, NewRoleWithId, RoleInfo, RolePageParams, SimpleRole, UpdateRole,
    },
    user::{
        ChangeUserPassword, ChangeUserPasswordWithId, ChangeUserStatus, NewUser, NewUserWithId,
        ResetUserPassword, UpdateUser, UserInfo, UserInfoWithRoles, UserPageParams,
    },
};

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct PathId {
    pub id: i32,
}
