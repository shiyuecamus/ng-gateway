//! Authentication middleware for handling bearer token authentication.
//! This module provides middleware that validates bearer tokens and attaches grant information to requests.

use actix_service::{Service, Transform};
use actix_web::{
    body::{EitherBody, MessageBody},
    dev::{ServiceRequest, ServiceResponse},
    error::ErrorInternalServerError,
    http::{header::AUTHORIZATION, Method},
    Error, HttpMessage, HttpResponse,
};
use futures::{
    future::{ok, LocalBoxFuture, Ready},
    FutureExt,
};
use jsonwebtoken::{Algorithm, Validation};
use ng_gateway_common::NGAppContext;
use ng_gateway_error::storage::{CacheError, StorageError};
use ng_gateway_models::{
    cache::{NGBaseCache, NGCacheExt, UserRoleCache, USER_ROLE_CACHE_NAME},
    constants::BEARER_TOKEN,
    domain::prelude::{Claims, SimpleRole},
    settings::Settings,
    web::WebResponse,
    CacheProvider,
};
use ng_gateway_repository::RoleRepository;
use ng_gateway_storage::NGCacheProvider;
use ng_gateway_utils::jwt::decode_jwt;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

/// Authentication middleware factory.
///
/// This struct is used to create new instances of the authentication middleware.
/// It implements the `Transform` trait to transform services into authenticated services.
pub struct Authentication;

impl<S, B> Transform<S, ServiceRequest> for Authentication
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: MessageBody,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthenticationMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(AuthenticationMiddleware {
            service: Rc::new(RefCell::new(service)),
        })
    }
}

/// Authentication middleware implementation.
///
/// This middleware handles bearer token authentication by:
/// 1. Extracting the bearer token from the Authorization header
/// 2. Validating the token against the access token cache
/// 3. Retrieving and attaching the associated grant information
/// 4. Allowing or denying the request based on authentication status
pub struct AuthenticationMiddleware<S> {
    service: Rc<RefCell<S>>,
}

impl<S, B> Service<ServiceRequest> for AuthenticationMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: MessageBody,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = S::Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let srv = self.service.clone();
        async move {
            // Fast path for OPTIONS requests
            if Method::OPTIONS == req.method() {
                return srv.call(req).await.map(|res| res.map_into_left_body());
            }
            // Try to authenticate with bearer token
            let token = match extract_bearer_token(&req) {
                Some(token) => token,
                None => {
                    return Ok(req
                        .into_response(HttpResponse::Unauthorized().json(WebResponse::<()>::error(
                            "Invalid token, please login again",
                        )))
                        .map_into_right_body())
                }
            };

            let settings = get_settings().await?;

            let mut validation = Validation::new(Algorithm::HS256);
            validation.validate_aud = false;
            validation.set_issuer(&[&settings.web.jwt.issuer]);

            let claims = match decode_jwt::<Claims>(
                token,
                settings.web.jwt.secret.as_bytes(),
                Some(validation),
            ) {
                Ok(td) => td.claims,
                Err(_) => {
                    return Ok(req
                        .into_response(HttpResponse::Unauthorized().json(WebResponse::<()>::error(
                            "Invalid token, please login again",
                        )))
                        .map_into_right_body())
                }
            };

            let user_id = claims.user_id.clone();

            // Insert grant info for authorization
            req.extensions_mut().insert(claims);

            let cache = get_user_role_cache().await?;
            let roles = cache
                .get_or_create(user_id, |key| async move {
                    let user_id = key.parse::<i32>().map_err(|e| {
                        StorageError::CacheKind(CacheError::Msg(format!("invalid user id: {e}")))
                    })?;
                    let roles = RoleRepository::find_user_role_infos(user_id)
                        .await?
                        .map(|roles| {
                            roles
                                .into_iter()
                                .map(|role| role.into())
                                .collect::<Vec<SimpleRole>>()
                        })
                        .unwrap_or_default();

                    Ok(UserRoleCache { roles })
                })
                .await
                .map_err(ErrorInternalServerError)?;

            req.extensions_mut().insert(roles);

            srv.call(req).await.map(|res| res.map_into_left_body())
        }
        .boxed_local()
    }
}

/// Extracts the bearer token from the request headers.
///
/// # Arguments
/// * `req` - The service request containing the headers
///
/// # Returns
/// * `Option<&str>` - The bearer token if present and valid, None otherwise
#[inline]
fn extract_bearer_token(req: &ServiceRequest) -> Option<&str> {
    req.headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix(BEARER_TOKEN)
        .map(str::trim)
}

/// Retrieves the settings from the application context.
///
/// # Returns
/// * `Result<Settings, Error>` - The settings if successful,
///   or an error if retrieval failed
#[inline]
async fn get_settings() -> Result<Settings, Error> {
    let ctx = NGAppContext::instance().await;
    ctx.settings().map_err(ErrorInternalServerError).cloned()
}

/// Retrieves the user role cache from the application context.
///
/// # Returns
/// * `Result<UserRoleCache, Error>` - The user role cache if successful,
///   or an error if retrieval failed
#[inline]
async fn get_user_role_cache(
) -> Result<Arc<dyn NGBaseCache<Value = UserRoleCache> + Send + Sync>, Error> {
    let ctx = NGAppContext::instance().await;
    let provider = ctx.cache_provider().map_err(ErrorInternalServerError)?;
    let cache_provider = provider
        .downcast_ref::<NGCacheProvider>()
        .ok_or(ErrorInternalServerError("Cache provider not initialized"))?;
    let cache = cache_provider
        .get_cache::<UserRoleCache>(USER_ROLE_CACHE_NAME)
        .map_err(ErrorInternalServerError)?;
    Ok(cache)
}
