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
        // ここでエラーの種類を判別して、適切なバリアントに変換する
        // ただし、今回は簡単のため、すべてを`ReadError`として扱う
        // 実際の実装では、エラーのコンテキストに応じて適切なバリアントを選択する必要がある
        // 例：error.kind() == std::io::ErrorKind::NotFound ならファイルが見つからないエラー
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
