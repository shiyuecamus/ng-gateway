use thiserror::Error;

#[derive(Default, Error, Debug)]
pub enum RBACError {
    #[default]
    #[error("primitive error")]
    Primitive,
    #[error("invalid value: {0}")]
    InvalidValue(String),
    #[error("invalid grant: {0}")]
    InvalidGrant(String),
    #[error("rule already exists for method '{method}' and path '{path}'")]
    RuleExists { method: String, path: String },
}
