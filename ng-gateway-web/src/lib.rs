//! Web server module for the NG Gateway application
mod api;
mod middleware;
mod rbac;
mod static_ui;
mod validation;

use actix_web::{
    dev::{Server, ServerHandle},
    middleware::{Compress, Logger, NormalizePath},
    web::{self, Data},
    App, HttpServer,
};
use async_trait::async_trait;
use middleware::cors::middleware;
use ng_gateway_error::{init::InitContextError, NGError, NGResult};
use ng_gateway_models::{
    settings::{SSLType, Settings, WebUiMode},
    Gateway, PermChecker, WebServer,
};
use ng_gateway_utils::tls::configure_rustls_server_config;
use static_ui::{UiAssets, UiRuntimeConfig};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, instrument};
use validation::{manager::ValidationManager, prelude::create_default_manager, EntityValidator};

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    validator: Arc<ValidationManager>,
    gateway: Arc<dyn Gateway>,
}

/// NGWebServer handles the web server initialization and management
#[derive(Clone)]
pub struct NGWebServer {
    /// Server handle for graceful shutdown
    server: Arc<Mutex<Option<ServerHandle>>>,
}

impl NGWebServer {
    /// Create and configure the HTTP server
    async fn create_server(
        settings: &Settings,
        perm_checker: Arc<dyn PermChecker>,
        gateway: Arc<dyn Gateway>,
    ) -> NGResult<Server> {
        // Initialize RBAC rules before starting the server
        api::init_rbac_rules(&settings.web.router_prefix, perm_checker)
            .await
            .map_err(|e| NGError::from(format!("Failed to initialize RBAC rules: {e}")))?;

        // Collect and register additional validators from API modules
        let mut validation_manager = create_default_manager();
        let additional_api_validators: Vec<Arc<dyn EntityValidator>> =
            api::collect_additional_validators();
        for validator in additional_api_validators {
            validation_manager.register(validator);
        }

        let addr = format!("{}:{}", settings.web.host, settings.web.port);
        let router_prefix = settings.web.router_prefix.clone();
        let worker_count = settings.web.get_worker_count();
        let cors_config = settings.web.cors.clone();

        // Prepare UI runtime config + assets upfront (outside actix `HttpServer::new` closure).
        // This avoids blocking work in the per-worker factory and keeps request handling lock-free.
        let ui_cfg = UiRuntimeConfig {
            api_router_prefix: router_prefix.clone(),
            ui: settings.web.ui.clone(),
        };
        let ui_enabled = ui_cfg.ui.enabled;
        let ui_embedded_zip_enabled =
            ui_enabled && matches!(ui_cfg.ui.mode, WebUiMode::EmbeddedZip);

        let ui_assets: Option<UiAssets> = if ui_embedded_zip_enabled {
            Some(UiAssets::load_from_embedded_zip().await?)
        } else {
            None
        };

        // Wrap UI config/assets into `Data` once and only clone the inner `Arc` per worker.
        // This avoids deep clones of `UiRuntimeConfig` and keeps the worker factory cheap.
        let ui_cfg_data = Data::new(ui_cfg);
        let ui_assets_data: Option<Data<UiAssets>> = ui_assets.map(Data::new);

        let state = AppState {
            validator: Arc::new(validation_manager),
            gateway,
        };

        let mut server = HttpServer::new(move || {
            let mut app = App::new()
                .app_data(Data::new(Arc::new(state.clone())))
                .wrap(middleware(&cors_config))
                .wrap(Logger::default())
                .wrap(Compress::default())
                .wrap(NormalizePath::trim())
                // Public root routes (not under `/api`).
                .configure(api::configure_public_routes)
                // Versioned API routes under router prefix (default: `/api`).
                .service(web::scope(&router_prefix).configure(api::configure_routes));

            // Register UI routes as the last-resort GET/HEAD handler (SPA + static files).
            // Best practice: a single switch (`web.ui.enabled`) controls on/off.
            if ui_enabled {
                app = app.app_data(ui_cfg_data.clone());
                if let Some(ref assets) = ui_assets_data {
                    app = app.app_data(assets.clone());
                }
                app = app.configure(static_ui::configure_ui_routes);
            }

            app
        })
        .workers(worker_count);

        // Configure SSL if enabled
        if settings.web.ssl.enabled {
            let https_addr = format!("{}:{}", settings.web.host, settings.web.ssl.port);
            let ca_cert_path = settings.general.ca_cert_path_resolved();
            let ca_key_path = settings.general.ca_key_path_resolved();
            server = server
                .bind_rustls_0_23(
                    &https_addr,
                    configure_rustls_server_config(
                        &settings.web.ssl.cert,
                        &settings.web.ssl.key,
                        matches!(settings.web.ssl.r#type, SSLType::Auto),
                        Some(&ca_cert_path),
                        Some(&ca_key_path),
                    )?,
                )
                .map_err(|e| {
                    NGError::from(format!("Failed to bind SSL server to {https_addr}: {e}"))
                })?;
        }

        // Bind HTTP server
        server = server
            .bind(&addr)
            .map_err(|e| NGError::from(format!("Failed to bind HTTP server to {addr}: {e}")))?;

        Ok(server.run())
    }
}

#[async_trait]
impl WebServer for NGWebServer {
    #[inline]
    #[instrument(name = "init-web-server", skip_all)]
    /// Initialize and start the web server
    async fn init(
        settings: &Settings,
        perm_checker: Arc<dyn PermChecker>,
        gateway: Arc<dyn Gateway>,
    ) -> NGResult<Arc<Self>, InitContextError> {
        let server = Self::create_server(settings, perm_checker, gateway)
            .await
            .map_err(|e| {
                InitContextError::Primitive(format!("Failed to create web server: {e}"))
            })?;
        let server_handle = server.handle();

        // Spawn server task
        tokio::spawn(async move {
            if let Err(e) = server.await {
                error!(error=%e, "Web server failed to start");
            }
        });

        let web_server = NGWebServer {
            server: Arc::new(Mutex::new(Some(server_handle))),
        };

        Ok(Arc::new(web_server))
    }

    #[inline]
    #[instrument(name = "web-server-stop", skip_all)]
    /// Gracefully stop the web server
    async fn stop(&self) -> NGResult<()> {
        info!("🛑 Stopping web server...");
        let mut server_guard = self.server.lock().await;
        if let Some(handle) = server_guard.take() {
            handle.stop(true).await;
        }
        info!("✅ Web server stopped successfully");

        Ok(())
    }
}
