use ng_gateway_common::NGAppContext;
use ng_gateway_error::storage::StorageError;
use ng_gateway_models::DbManager;
use ng_gateway_storage::NGDbManager;
use sea_orm::DatabaseConnection;

pub mod action;
pub mod app;
pub mod app_ext;
pub mod app_sub;
pub mod branding;
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

pub use action::ActionRepository;
pub use app::AppRepository;
pub use app_ext::AppExtRepository;
pub use app_sub::AppSubRepository;
pub use branding::BrandingRepository;
pub use channel::ChannelRepository;
pub use credentials::CredentialsRepository;
pub use device::DeviceRepository;
pub use driver::DriverRepository;
pub use menu::MenuRepository;
pub use plugin::PluginRepository;
pub use point::PointRepository;
pub use relation::RelationRepository;
pub use role::RoleRepository;
pub use user::UserRepository;

#[inline]
pub async fn get_db_connection() -> Result<DatabaseConnection, StorageError> {
    let ctx = NGAppContext::instance().await;
    ctx.db_manager()
        .map_err(|_| StorageError::StorageUnavailable)?
        .downcast_ref::<NGDbManager>()
        .ok_or(StorageError::StorageUnavailable)?
        .get_connection()
}
