// Constants for the ng modules
// This file contains global constants used across the application

/// The default configuration file name for the application.
/// This constant is used to specify the default configuration file
/// that the application will attempt to load at startup.
pub const DEFAULT_CONFIG_FILE_NAME: &str = "gateway.toml";

/// The default casbin model.
/// This constant is used to specify the default casbin model
/// that the application will attempt to load at startup.
pub const CASBIN_MODEL: &str = r#"
[request_definition]
r = sub, obj, act, typ

[policy_definition]
p = sub, obj, act, typ

[role_definition]
g = _, _

[policy_effect]
e = some(where (p.eft == allow))

[matchers]
m = (p.typ == "resource" && g(r.sub, p.sub) && r.obj == p.obj && r.act == p.act) || \
    (p.typ == "api" && g(r.sub, p.sub) && r.obj == p.obj && r.act == p.act) || \
    (p.typ == "scope" && g(r.sub, p.sub) && r.obj == p.obj && r.act == "access") || \
    r.sub == "system_admin"
"#;

pub const BEARER_TOKEN: &str = "Bearer";

pub const SYSTEM_ADMIN_ROLE_CODE: &str = "SYSTEM_ADMIN";
pub const SYSTEM_ADMIN_ROLE_NAME: &str = "系统管理员";

pub const DATA_DIR: &str = "./data";
pub const CERT_DIR: &str = "./certs";
pub const PKI_DIR: &str = "./pki";
pub const DRIVER_DIR: &str = "./drivers";
pub const PLUGIN_DIR: &str = "./plugins";
pub const BUILTIN_DIR: &str = "builtin";
pub const CUSTOM_DIR: &str = "custom";
