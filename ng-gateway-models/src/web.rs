use actix_web::body::EitherBody;
use actix_web::{HttpResponse, Responder};
use serde::Deserialize;
use serde::Serialize;

/// Response code
pub enum ResponseCode {
    /// Success
    Success = 0,
    /// Error
    Error = 500,
}

/// Standard response structure for all REST API endpoints
#[derive(Debug, Serialize, Deserialize)]
pub struct WebResponse<T> {
    /// Response code
    pub code: u16,
    /// Human-readable message describing the result
    pub message: String,
    /// Optional payload data (present on success, may be absent on errors)
    pub data: Option<T>,
}

impl<T> WebResponse<T> {
    /// Create a new response with specified message and optional data
    pub fn new(code: ResponseCode, message: &str, data: Option<T>) -> Self {
        Self {
            code: code as u16,
            message: message.into(),
            data,
        }
    }

    /// Create a success response with data
    pub fn ok(data: T) -> Self {
        Self {
            code: ResponseCode::Success as u16,
            message: "success".into(),
            data: Some(data),
        }
    }

    /// Create a success response with message and data
    pub fn ok_with_message(message: &str, data: T) -> Self {
        Self {
            code: ResponseCode::Success as u16,
            message: message.into(),
            data: Some(data),
        }
    }

    /// Create an empty success response (no data)
    pub fn ok_empty() -> WebResponse<()> {
        WebResponse {
            code: ResponseCode::Success as u16,
            message: "success".into(),
            data: None,
        }
    }

    /// Create an error response with message
    pub fn error(message: &str) -> Self {
        Self {
            code: ResponseCode::Error as u16,
            message: message.into(),
            data: None,
        }
    }
}

/// Implement Responder for WebResponse<T> so it can be returned from actix-web handlers
impl<T> Responder for WebResponse<T>
where
    T: Serialize,
{
    type Body = EitherBody<String>;

    fn respond_to(self, _req: &actix_web::HttpRequest) -> HttpResponse<EitherBody<String>> {
        HttpResponse::Ok()
            .content_type("application/json")
            .body(serde_json::to_string(&self).unwrap())
            .map_into_right_body()
    }
}
