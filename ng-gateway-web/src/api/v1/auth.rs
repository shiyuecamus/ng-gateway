use actix_web::{web, HttpRequest};
use actix_web_validator::Json;
use ng_gateway_common::NGAppContext;
use ng_gateway_error::{web::WebError, WebResult};
use ng_gateway_models::{
    domain::prelude::{Claims, LoginRequest, LoginResponse},
    enums::common::{EntityType, Status},
    web::WebResponse,
};
use ng_gateway_repository::UserRepository;
use ng_gateway_utils::{hash::bcrypt_check, jwt::encode_jwt};

pub(super) const ROUTER_PREFIX: &str = "/auth";

/// Configure authentication routes
///
/// # Description
/// Registers all authentication endpoints with the Actix web service
///
/// # Routes
/// - POST `/login`: Login endpoint
/// - POST `/logout`: Logout endpoint
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/logout", web::post().to(logout));
}

/// Login endpoint
///
/// # Endpoint
/// `POST /api/auth/login`
///
/// # Description
/// Logs in the current user
///
/// # Returns
/// - `Result<WebResponse<LoginResponse>, WebError>`: Login response or error
pub async fn login(req: Json<LoginRequest>) -> WebResult<WebResponse<LoginResponse>> {
    let username = req.username.as_ref().unwrap();
    let password = req.password.as_ref().unwrap();

    let user = match UserRepository::find_by_username_and_status(username, Status::Enabled).await? {
        Some(user) => user,
        None => return Err(WebError::NotFound(EntityType::User.to_string())),
    };

    if !bcrypt_check(password, &user.password) {
        return Err(WebError::Unauthorized);
    }

    let settings = NGAppContext::instance()
        .await
        .settings()
        .map_err(|_| WebError::InternalError("Failed to get settings".to_string()))?
        .clone();

    let claims = Claims::new(
        settings.web.jwt.issuer.clone(),
        None,
        user.id.to_string(),
        user.username.clone(),
        settings.web.jwt.expire,
    );

    let token = encode_jwt(&claims, settings.web.jwt.secret.as_bytes(), None)
        .map_err(|_| WebError::InternalError("Failed to encode JWT".to_string()))?;

    Ok(WebResponse::ok(LoginResponse {
        jti: claims.jti,
        sub: claims.sub,
        iss: claims.iss,
        aud: claims.aud,
        exp: claims.exp,
        nbf: claims.nbf,
        iat: claims.iat,
        user_id: claims.user_id,
        username: claims.username,
        token,
        access_token_expire: settings.web.jwt.expire,
    }))
}

/// Logout endpoint
///
/// # Endpoint
/// `POST /api/auth/logout`
///
/// # Description
/// Logs out the current user
///
/// # Returns
/// - `Result<WebResponse<bool>, WebError>`: Logout response or error
async fn logout(_req: HttpRequest) -> WebResult<WebResponse<bool>> {
    // TODO: send user logout event
    Ok(WebResponse::ok(true))
}
