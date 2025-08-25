use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionError {
    #[error("Session not found: {0}")]
    NotFound(String),

    #[error("Invalid session ID: {0}")]
    InvalidId(String),

    #[error("Failed to read session data: {0}")]
    ReadError(std::io::Error),
    #[error("Failed to parse session data: {0}")]
    ParseError(#[from] serde_json::Error),
    #[error("Failed to create session directory: {0}")]
    CreateDirError(std::io::Error),
    #[error("Failed to write session data: {0}")]
    WriteError(std::io::Error),
    #[error("Failed to delete session directory: {0}")]
    DeleteError(std::io::Error),
    #[error("Failed to generate UUID: {0}")]
    UuidError(#[from] uuid::Error),
}

impl From<std::io::Error> for SessionError {
    fn from(error: std::io::Error) -> Self {
        // Here, we determine the type of error and convert it to the appropriate variant
        // For simplicity, we'll treat all errors as `ReadError`
        // In a real implementation, we would need to select the appropriate variant based on the error context
        // For example: error.kind() == std::io::ErrorKind::NotFound would be a file not found error
        SessionError::ReadError(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_error_display() {
        let error = SessionError::NotFound("test-id".to_string());
        assert_eq!(format!("{}", error), "Session not found: test-id");

        let error = SessionError::InvalidId("".to_string());
        assert_eq!(format!("{}", error), "Invalid session ID: ");
    }
}
