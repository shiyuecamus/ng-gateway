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
    driver::{DriverInfo, DriverPageParams, NewDriver, PathEntityId, TemplateQuery, UpdateDriver},
    import::{
        CommitResult, DeviceGroup, DeviceRef, ImportPreview, PreparedActionCommit,
        PreparedDeviceCommit, PreparedDevicePointsCommit, PreparedPointCommit,
    },
    menu::{ChangeMenuStatus, MenuInfo, MenuMeta, MenuTree, NewMenu, NewMenuWithId, UpdateMenu},
    net_debug::{
        HttpMethod, HttpRequest, HttpResponse, PingMode, PingRequest, PingResponse, PingSample,
        TcpConnectRequest, TcpConnectResponse,
    },
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
