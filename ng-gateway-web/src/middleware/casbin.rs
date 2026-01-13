//! Casbin middleware for handling authorization.
//! This module provides middleware that enforces access control policies using Casbin.

use actix_service::{Service, Transform};
use actix_web::{
    body::{EitherBody, MessageBody},
    dev::{ServiceRequest, ServiceResponse},
    error::ErrorInternalServerError,
    Error, HttpMessage, HttpResponse,
};
use futures::{
    future::{ok, LocalBoxFuture, Ready},
    FutureExt,
};
use ng_gateway_common::{casbin::NGPermChecker, NGAppContext};
use ng_gateway_error::NGError;
use ng_gateway_models::{domain::prelude::Claims, web::WebResponse, PermChecker};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

/// Casbin middleware factory.
///
/// This struct is used to create new instances of the Casbin middleware.
/// It implements the `Transform` trait to transform services into authorized services.
#[derive(Clone)]
pub struct CasbinService;

impl<S, B> Transform<S, ServiceRequest> for CasbinService
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: MessageBody,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = CasbinMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ok(CasbinMiddleware {
            service: Rc::new(RefCell::new(service)),
        })
    }
}

/// Casbin middleware implementation.
///
/// This middleware handles authorization by:
/// 1. Extracting the grant information from the request
/// 2. Checking permissions using the Casbin policy checker
/// 3. Allowing or denying the request based on authorization status
#[derive(Clone)]
pub struct CasbinMiddleware<S> {
    service: Rc<RefCell<S>>,
}

impl<S, B> Service<ServiceRequest> for CasbinMiddleware<S>
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
            let path = req.match_pattern().unwrap_or_default();
            let action = req.method().as_str();
            let option_grant = req.extensions().get::<Claims>().cloned();
            let claims = match option_grant {
                Some(value) => Arc::new(value),
                None => {
                    return Ok(req.into_response(
                        HttpResponse::Unauthorized()
                            .json(WebResponse::<()>::error("Authentication required"))
                            .map_into_right_body(),
                    ))
                }
            };
            let perm_checker = get_perm_checker().await?;
            match perm_checker.check(action, &path, claims).await {
                Ok(checked) => match checked {
                    true => srv.call(req).await.map(|res| res.map_into_left_body()),
                    false => Ok(req.into_response(
                        HttpResponse::Forbidden()
                            .json(WebResponse::<()>::error("Permission denied"))
                            .map_into_right_body(),
                    )),
                },
                Err(e) => {
                    let res = HttpResponse::InternalServerError()
                        .json(WebResponse::<()>::error(&e.to_string()))
                        .map_into_right_body();
                    Ok(req.into_response(res))
                }
            }
        }
        .boxed_local()
    }
}

/// Retrieves the permission checker from the application context.
///
/// # Returns
/// * `Result<Arc<NGPermChecker>, Error>` - The permission checker if successful,
///   or an error if retrieval failed
async fn get_perm_checker() -> Result<Arc<NGPermChecker>, Error> {
    let ctx = NGAppContext::instance().await;
    let checker = ctx.perm_checker().map_err(ErrorInternalServerError)?;
    checker
        .downcast_arc::<NGPermChecker>()
        .map_err(|_| ErrorInternalServerError(NGError::from("Permission checker not initialized")))
}
