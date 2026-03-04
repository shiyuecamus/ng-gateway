//! AI engine REST API routes.

use crate::rbac::{has_any_role, has_resource_operation, has_scope};
use actix_web::{
    http::Method,
    web::{self, ServiceConfig},
};
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, NGResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    PermChecker,
};
use tracing::{info, instrument};

mod algorithms;
mod common;
mod models;
mod pipelines;
mod runtime;

pub(super) const ROUTER_PREFIX: &str = "/ai";

// ── Route configuration ───────────────────────────────────────────

/// Configure all AI API routes.
///
/// # Phase 1 Routes
/// - GET `/models` — list all models
/// - GET `/models/{model_id}` — model details
/// - POST `/models` — upload model
/// - PUT `/models/{model_id}` — update model config
/// - DELETE `/models/{model_id}` — delete model
/// - POST `/models/{model_id}/load` — hot load model
/// - POST `/models/{model_id}/unload` — hot unload model
/// - GET `/pipelines` — list all pipelines
/// - GET `/pipelines/{id}` — pipeline details
/// - POST `/pipelines/{id}/validate` — validate pipeline DAG constraints
/// - POST `/pipelines` — create pipeline binding
/// - PUT `/pipelines` — update pipeline binding
/// - DELETE `/pipelines/{id}` — delete pipeline binding
/// - GET `/engine/status` — engine global status
/// - GET `/channels/{id}/snapshot` — latest annotated JPEG snapshot
/// - GET `/processors/pre` — available preprocessors
/// - GET `/processors/post` — available postprocessors
///
/// # Phase 2 Routes
/// - GET `/algorithms` — list all algorithms
/// - GET `/algorithms/{id}` — algorithm details
/// - POST `/algorithms/probe` — probe WASM algorithm (multipart)
/// - POST `/algorithms/install` — install WASM algorithm (multipart)
/// - DELETE `/algorithms/{id}` — delete algorithm
/// - POST `/algorithms/{id}/test` — test algorithm with mock data
pub(crate) fn configure_routes(cfg: &mut ServiceConfig) {
    cfg
        // Model management — probe + install paradigm (aligned with driver/plugin)
        .route("/models/probe", web::post().to(models::probe_model))
        .route("/models/install", web::post().to(models::install_model))
        .route("/models/list", web::get().to(models::list_models))
        .route("/models/page", web::get().to(models::page_models))
        .route("/models/detail/{id}", web::get().to(models::get_model))
        .route("/models/{id}", web::put().to(models::update_model))
        .route("/models/{id}", web::delete().to(models::uninstall_model))
        .route("/models/{id}/load", web::post().to(models::load_model))
        .route("/models/{id}/unload", web::post().to(models::unload_model))
        // Pipeline management (aligned with driver/plugin)
        .route("/pipelines/list", web::get().to(pipelines::list_pipelines))
        .route("/pipelines/page", web::get().to(pipelines::page_pipelines))
        .route(
            "/pipelines/detail/{id}",
            web::get().to(pipelines::get_pipeline),
        )
        .route(
            "/pipelines/{id}/validate",
            web::post().to(pipelines::validate_pipeline),
        )
        .route("/pipelines", web::post().to(pipelines::create_pipeline))
        .route("/pipelines", web::put().to(pipelines::update_pipeline))
        .route(
            "/pipelines/{id}",
            web::delete().to(pipelines::delete_pipeline),
        )
        .route("/engine/status", web::get().to(runtime::get_engine_status))
        .route(
            "/channels/{id}/snapshot",
            web::get().to(runtime::get_snapshot),
        )
        .route(
            "/processors/pre",
            web::get().to(runtime::list_preprocessors),
        )
        .route(
            "/processors/post",
            web::get().to(runtime::list_postprocessors),
        )
        // Algorithm management — probe + install (aligned with driver/plugin)
        .route(
            "/algorithms/probe",
            web::post().to(algorithms::probe_algorithm),
        )
        .route(
            "/algorithms/install",
            web::post().to(algorithms::install_algorithm),
        )
        .route(
            "/algorithms/list",
            web::get().to(algorithms::list_algorithms),
        )
        .route(
            "/algorithms/page",
            web::get().to(algorithms::page_algorithms),
        )
        .route(
            "/algorithms/detail/{id}",
            web::get().to(algorithms::get_algorithm),
        )
        .route(
            "/algorithms/{id}",
            web::delete().to(algorithms::uninstall_algorithm),
        )
        .route(
            "/algorithms/{id}/test",
            web::post().to(algorithms::test_algorithm),
        );
}

/// Initialize RBAC rules for AI module.
#[inline]
#[instrument(name = "init-ai-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> NGResult<(), RBACError> {
    let model_read = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(
                r.or(has_resource_operation(EntityType::Model, Operation::Read)?)
                    .or(has_scope("ai:model:read")?),
            )
        })
    };
    let model_write = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(
                r.or(has_resource_operation(EntityType::Model, Operation::Write)?)
                    .or(has_scope("ai:model:write")?),
            )
        })
    };
    let model_create = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Model,
                Operation::Create,
            )?)
            .or(has_scope("ai:model:create")?))
        })
    };
    let model_delete = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Model,
                Operation::Delete,
            )?)
            .or(has_scope("ai:model:delete")?))
        })
    };

    let pipeline_read = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Pipeline,
                Operation::Read,
            )?)
            .or(has_scope("ai:pipeline:read")?))
        })
    };
    let pipeline_create = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Pipeline,
                Operation::Create,
            )?)
            .or(has_scope("ai:pipeline:create")?))
        })
    };
    let pipeline_write = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Pipeline,
                Operation::Write,
            )?)
            .or(has_scope("ai:pipeline:write")?))
        })
    };
    let pipeline_delete = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Pipeline,
                Operation::Delete,
            )?)
            .or(has_scope("ai:pipeline:delete")?))
        })
    };

    let algorithm_read = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Algorithm,
                Operation::Read,
            )?)
            .or(has_scope("ai:algorithm:read")?))
        })
    };
    let algorithm_create = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Algorithm,
                Operation::Create,
            )?)
            .or(has_scope("ai:algorithm:create")?))
        })
    };
    let algorithm_write = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Algorithm,
                Operation::Write,
            )?)
            .or(has_scope("ai:algorithm:write")?))
        })
    };
    let algorithm_delete = || {
        has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| {
            Ok(r.or(has_resource_operation(
                EntityType::Algorithm,
                Operation::Delete,
            )?)
            .or(has_scope("ai:algorithm:delete")?))
        })
    };

    let rules = vec![
        // Model management — probe + install
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/models/probe"),
            model_read()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/models/install"),
            model_create()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/models/list"),
            model_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/models/page"),
            model_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/models/detail/{{id}}"),
            model_read()?,
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{id}}"),
            model_write()?,
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{id}}"),
            model_delete()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{id}}/load"),
            model_write()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{id}}/unload"),
            model_write()?,
        ),
        // Pipeline management
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/list"),
            pipeline_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/page"),
            pipeline_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/detail/{{id}}"),
            pipeline_read()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/{{id}}/validate"),
            pipeline_write()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines"),
            pipeline_create()?,
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines"),
            pipeline_write()?,
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/{{id}}"),
            pipeline_delete()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/engine/status"),
            pipeline_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/channels/{{id}}/snapshot"),
            pipeline_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/processors/pre"),
            pipeline_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/processors/post"),
            pipeline_read()?,
        ),
        // Algorithm management — probe + install
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/probe"),
            algorithm_read()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/install"),
            algorithm_create()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/list"),
            algorithm_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/page"),
            algorithm_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/detail/{{id}}"),
            algorithm_read()?,
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/{{id}}"),
            algorithm_delete()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/{{id}}/test"),
            algorithm_write()?,
        ),
    ];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    info!("AI module RBAC rules initialized successfully");
    Ok(())
}
