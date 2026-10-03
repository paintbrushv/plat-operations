//! Released USD operating calculations. Legacy floating-point features are separate.
pub mod migration;
pub mod money;
pub mod protocol;
pub mod store;
pub mod variance;

pub const CONTRACT: &str = "plat.ops/1";
pub const SCHEMA: i64 = 1;
pub const APPLICATION_ID: i64 = 0x504c4154;

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ExactError {
    pub code: &'static str,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, ExactError>;
pub fn error(code: &'static str, message: impl Into<String>) -> ExactError {
    ExactError {
        code,
        message: message.into(),
    }
}
impl From<sqlx::Error> for ExactError {
    fn from(_: sqlx::Error) -> Self {
        error(
            "DATABASE_ERROR",
            "Database operation refused; schema, revision, or write conflict",
        )
    }
}
impl From<std::io::Error> for ExactError {
    fn from(_: std::io::Error) -> Self {
        error("IO_ERROR", "Local file operation failed")
    }
}
impl From<serde_json::Error> for ExactError {
    fn from(_: serde_json::Error) -> Self {
        error("INVALID_INPUT", "Input does not match the versioned schema")
    }
}
