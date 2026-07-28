use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("validation error ({0})")]
    Validation(String),
    #[error("missing required field ({0})")]
    MissingField(String),
    #[error("invalid value for {field} ({message})")]
    InvalidValue { field: String, message: String },
    #[error("io error ({0})")]
    Io(std::io::Error),
}
