pub mod action;
pub mod app;
pub mod app_ext;
pub mod app_sub;
pub mod branding;
pub mod casbin;
pub mod channel;
pub mod credentials;
pub mod device;
pub mod driver;
pub mod menu;
pub mod plugin;
pub mod point;
pub mod relation;
pub mod role;
pub mod user;

#[allow(unused)]
const INIT_SYSTEM_ORDER: i32 = 0;
const INIT_MENU_ORDER: i32 = INIT_SYSTEM_ORDER + 1;
const INIT_ROLE_ORDER: i32 = INIT_MENU_ORDER + 1;
const INIT_USER_ORDER: i32 = INIT_ROLE_ORDER + 1;

const INIT_INTERNAL_ORDER: i32 = 100;
const INIT_CREDENTIALS_ORDER: i32 = INIT_INTERNAL_ORDER + 1;
const INIT_DRIVER_ORDER: i32 = INIT_CREDENTIALS_ORDER - 1;
const INIT_CHANNEL_ORDER: i32 = INIT_DRIVER_ORDER + 1;
const INIT_DEVICE_ORDER: i32 = INIT_CHANNEL_ORDER + 1;
const INIT_POINT_ORDER: i32 = INIT_DEVICE_ORDER + 1;
const INIT_ACTION_ORDER: i32 = INIT_POINT_ORDER + 1;

const INIT_PLUGIN_ORDER: i32 = INIT_ACTION_ORDER + 1;
const INIT_APP_ORDER: i32 = INIT_PLUGIN_ORDER + 1;
const INIT_APP_EXT_ORDER: i32 = INIT_APP_ORDER + 1;
const INIT_APP_SUB_ORDER: i32 = INIT_APP_EXT_ORDER + 1;

#[allow(unused)]
const INIT_LATEST_ORDER: i32 = 10000;
const INIT_RELATION_ORDER: i32 = INIT_LATEST_ORDER + 1;
const INIT_CASBIN_ORDER: i32 = INIT_RELATION_ORDER + 1;
