use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Serialize, Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(required(message = "username is required"))]
    pub username: Option<String>,
    #[validate(required(message = "password is required"))]
    pub password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LoginResponse {
    pub jti: String,
    pub sub: String,
    pub iss: String,
    pub aud: Option<Vec<String>>,
    pub exp: i64,
    pub nbf: i64,
    pub iat: i64,
    pub user_id: String,
    pub username: String,
    pub token: String,
    pub access_token_expire: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub jti: String,
    pub sub: String,
    pub iss: String,
    pub aud: Option<Vec<String>>,
    pub exp: i64,
    pub nbf: i64,
    pub iat: i64,
    pub user_id: String,
    pub username: String,
    pub access_token_expire: i64,
}

impl Claims {
    pub fn new(
        iss: String,
        aud: Option<Vec<String>>,
        user_id: String,
        username: String,
        access_token_expire: i64,
    ) -> Self {
        let jti = Uuid::new_v4().into();
        let now = Utc::now();
        Self {
            jti,
            sub: user_id.clone(),
            iss,
            aud,
            exp: now.timestamp() + access_token_expire,
            nbf: now.timestamp(),
            iat: now.timestamp(),
            user_id,
            username,
            access_token_expire,
        }
    }
}
