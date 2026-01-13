use actix_web::{HttpResponse, ResponseError};
use serde_json::json;
use thiserror::Error;

use crate::{storage::StorageError, NGError};

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Unauthorized")]
    Unauthorized,
    #[error("BadRequest: `{0}`")]
    BadRequest(String),
    #[error("`{0}` not found")]
    NotFound(String),
    #[error("Forbidden: `{0}`")]
    Forbidden(String),
    #[error("InternalError: `{0}`")]
    InternalError(String),
    #[error("DBError: `{0}`")]
    StorageError(#[from] StorageError),
    #[error("MultipartError: `{0}`")]
    MultipartError(String),
}

impl From<std::io::Error> for WebError {
    fn from(e: std::io::Error) -> Self {
        WebError::InternalError(e.to_string())
    }
}

impl From<NGError> for WebError {
    fn from(e: NGError) -> Self {
        match e {
            NGError::StorageError(StorageError::EntityNotFound(msg)) => WebError::NotFound(msg),
            NGError::Timeout(dur) => {
                WebError::BadRequest(format!("Timeout: {} ms", dur.as_millis()))
            }
            other => WebError::InternalError(other.to_string()),
        }
    }
}

impl From<actix_multipart::MultipartError> for WebError {
    fn from(e: actix_multipart::MultipartError) -> Self {
        WebError::MultipartError(e.to_string())
    }
}

impl ResponseError for WebError {
    fn error_response(&self) -> HttpResponse {
        let mut body = json!({
            "message": self.to_string()
        });
        match self {
            WebError::Unauthorized => {
                body["error"] = json!("Unauthorized");
                HttpResponse::Unauthorized().json(body)
            }
            WebError::BadRequest(_) => {
                body["error"] = json!("Bad Request");
                HttpResponse::BadRequest().json(body)
            }
            WebError::NotFound(_) => {
                body["error"] = json!("Not Found");
                HttpResponse::NotFound().json(body)
            }
            WebError::Forbidden(_) => {
                body["error"] = json!("Forbidden");
                HttpResponse::Forbidden().json(body)
            }
            WebError::InternalError(_) => {
                body["error"] = json!("Internal Server Error");
                HttpResponse::InternalServerError().json(body)
            }
            WebError::StorageError(_) => {
                body["error"] = json!("Storage Error");
                HttpResponse::InternalServerError().json(body)
            }
            WebError::MultipartError(msg) => {
                body["error"] = json!("Multipart Error");
                body["message"] = json!(msg);
                HttpResponse::InternalServerError().json(body)
            }
        }
    }
}
