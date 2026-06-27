use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Missing required field: {0}")]
    MissingField(String),
    #[error("Invalid value for {field}: {message}")]
    InvalidValue { field: String, message: String },
    #[error("IO error: {0}")]
    Io(std::io::Error),
}
