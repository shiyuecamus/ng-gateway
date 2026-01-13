use actix_cors::Cors;
use actix_web::http::Method;
use ng_gateway_models::settings::{Cors as NGCors, CorsMode};

pub(crate) fn middleware(cors_config: &NGCors) -> Cors {
    match cors_config.mode {
        CorsMode::AllowAll => {
            // Log only once for all worker threads
            Cors::default()
                .allow_any_origin()
                .allow_any_method()
                .allow_any_header()
                .supports_credentials()
                .max_age(3600)
        }
        CorsMode::Whitelist => {
            // Log only once for all worker threads
            let origins = cors_config.whitelist.origins.clone();
            let mut cors = Cors::default()
                .allowed_origin_fn(move |origin, _| {
                    origins.contains(&origin.to_str().unwrap().to_string())
                })
                .allowed_methods(
                    cors_config
                        .whitelist
                        .methods
                        .iter()
                        .filter_map(|m| Method::from_bytes(m.as_bytes()).ok())
                        .collect::<Vec<Method>>(),
                )
                .allowed_headers(cors_config.whitelist.headers.clone())
                .expose_headers(cors_config.whitelist.expose_headers.clone())
                .max_age(3600);

            if cors_config.whitelist.credentials {
                cors = cors.supports_credentials();
            }
            cors
        }
    }
}
