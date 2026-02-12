use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Claude projects directory not found at {0}")]
    ProjectsDirNotFound(String),

    #[error("No conversation history found in {0}")]
    NoHistoryFound(String),

    #[error("User cancelled selection")]
    SelectionCancelled,

    #[error("Failed to run Claude CLI: {0}")]
    ClaudeExecutionError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Invalid argument: {0}")]
    InvalidArgs(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
