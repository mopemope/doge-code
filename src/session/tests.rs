#[cfg(test)]
use crate::session::{SessionManager, SessionStore};
use tempfile::tempdir;

#[test]
fn test_session_metric_tracking() {
    // Create a temporary directory for the test
    let temp_dir = tempdir().expect("Failed to create temp directory");
    let temp_path = temp_dir.path().to_path_buf();

    // Create a SessionStore using the temp directory
    let store = SessionStore::new(temp_path.join(".doge/sessions"))
        .expect("Failed to create session store");

    // Create a SessionManager
    let mut session_manager = SessionManager {
        store,
        current_session: None,
    };

    // Create a new session
    session_manager
        .create_session()
        .expect("Failed to create session");
    assert!(session_manager.current_session.is_some());

    // Check initial values
    let session = session_manager.current_session.as_ref().unwrap();
    assert_eq!(session.token_count, 0);
    assert_eq!(session.requests, 0);
    assert_eq!(session.tool_calls, 0);

    // Test incrementing token count
    session_manager
        .update_current_session_with_token_count(100)
        .expect("Failed to update token count");
    let session = session_manager.current_session.as_ref().unwrap();
    assert_eq!(session.token_count, 100);

    // Test incrementing request count
    session_manager
        .update_current_session_with_request_count()
        .expect("Failed to update request count");
    let session = session_manager.current_session.as_ref().unwrap();
    assert_eq!(session.requests, 1);

    // Test incrementing tool call count
    session_manager
        .update_current_session_with_tool_call_count()
        .expect("Failed to update tool call count");
    let session = session_manager.current_session.as_ref().unwrap();
    assert_eq!(session.tool_calls, 1);

    // Test multiple increments
    session_manager
        .update_current_session_with_token_count(50)
        .expect("Failed to update token count");
    session_manager
        .update_current_session_with_request_count()
        .expect("Failed to update request count");
    session_manager
        .update_current_session_with_tool_call_count()
        .expect("Failed to update tool call count");

    let session = session_manager.current_session.as_ref().unwrap();
    assert_eq!(session.token_count, 150); // 100 + 50
    assert_eq!(session.requests, 2); // 1 + 1
    assert_eq!(session.tool_calls, 2); // 1 + 1
}
