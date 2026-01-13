pub(crate) mod auth;
pub(crate) mod casbin;
pub(crate) mod cors;

use actix_web::{dev::Payload, Error, FromRequest, HttpMessage, HttpRequest};
use futures::future::{ready, Ready};
use ng_gateway_models::{cache::UserRoleCache, domain::prelude::Claims};

#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub grant: Option<Claims>,
    pub roles: Option<UserRoleCache>,
}

impl FromRequest for RequestContext {
    type Error = Error;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let mut ctx = RequestContext::default();
        if let Some(grant) = req.extensions().get::<Claims>().cloned() {
            ctx.grant = Some(grant);
        }
        if let Some(roles) = req.extensions().get::<UserRoleCache>().cloned() {
            ctx.roles = Some(roles);
        }
        ready(Ok(ctx))
    }
}
