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
        SortOrder, SortParams,
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
    network::{
        ApAction, ApMode, ApStatus, ConfigureApRequest, ConfigureInterfaceRequest,
        ControlApRequest, ForgetWifiRequest, InterfaceKind, InterfaceNamePath, IpConfig, IpMethod,
        Ipv4AddressInfo, Ipv4Config, Ipv6AddressInfo, Ipv6Config, LinkState, NetworkCapabilities,
        NetworkInterfaceDetail, NetworkInterfaceSummary, PlatformSupport, SavedWifiConnection,
        StaApCapability, StaticIpConfig, WifiAccessPoint, WifiBand, WifiConnectPreflight,
        WifiConnectRequest, WifiDisconnectRequest, WifiInterfaceQuery, WifiMode, WifiScanResult,
        WifiScanStatus, WifiSecurity, WifiStaStatus, WifiUuidPath, WiredStatus,
        WirelessInterfaceCapability,
    },
    plugin::{NewPlugin, PluginInfo, PluginPageParams, UpdatePlugin},
    point::{
        NewPoint, PointInfo, PointPageParams, UpdatePoint, WritePointPayload, WritePointResult,
    },
    relation::{NewRelation, RelationDelete, RelationInfo, RelationQuery, UpdateRelation},
    role::{
        ChangeRoleStatus, NewRole, NewRoleWithId, RoleInfo, RolePageParams, SimpleRole, UpdateRole,
    },
    system_settings::{
        AppLogLevelView, AppLogOverrideView, ApplySystemSettingsResult, ChannelLogLevelView,
        ChannelLogOverrideView, CleanupLogFilesRequest, CleanupLogFilesResponse,
        CollectorSettingsView, DownloadLogFilesRequest, GlobalLogLevelView, LogFileInfo,
        LogFilesListResponse, LogLevel, LoggingCleanupSettingsView, LoggingControlSettingsView,
        LoggingFileOutputSettingsView, LoggingFileRetentionSettingsView,
        LoggingFileRotationSettingsView, LoggingOutputFormat, LoggingOutputSettingsView,
        LoggingRotationMode, LoggingTimeRotation, NorthwardSettingsView,
        PatchCollectorSettingsRequest, PatchLoggingCleanupSettingsRequest,
        PatchLoggingControlSettingsRequest, PatchLoggingFileOutputRequest,
        PatchLoggingFileRetentionRequest, PatchLoggingFileRotationRequest,
        PatchLoggingOutputSettingsRequest, PatchNorthwardSettingsRequest, PatchRetryPolicyRequest,
        PatchSouthwardSettingsRequest, RetryPolicySettingsView, RuntimeSettingKey,
        SetAppLogLevelRequest, SetChannelLogLevelRequest, SetGlobalLogLevelRequest, SettingField,
        SettingValueSource, SouthwardSettingsView, SystemSettingsDomain, SystemSettingsImpact,
        SystemSettingsOverviewView, TtlRange,
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
