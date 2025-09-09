#[cfg(test)]
mod tests {
    use crate::session::SessionStore;
    use crate::tools::FsTools;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;
    use tokio::sync::RwLock;

    #[test]
    fn test_fs_tools_with_session_manager() {
        // Create a temporary directory for the test
        let temp_dir = tempdir().expect("Failed to create temp directory");
        let temp_path = temp_dir.path().to_path_buf();

        // Create a SessionStore using the temp directory
        let store = SessionStore::new(temp_path.join(".doge/sessions"))
            .expect("Failed to create session store");

        // Create a SessionManager
        let session_manager = Arc::new(Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));

        // Create FsTools with session manager
        let repomap = Arc::new(RwLock::new(None));
        let fs_tools = FsTools::new(repomap).with_session_manager(session_manager.clone());

        // Test that session manager is set
        assert!(fs_tools.session_manager.is_some());

        // Test update_session_with_tool_call_count
        // First create a session
        {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr
                .create_session()
                .expect("Failed to create session");
        }

        // Then test the update method
        assert!(fs_tools.update_session_with_tool_call_count().is_ok());

        // Check that the session was updated
        {
            let session_mgr = session_manager.lock().unwrap();
            let session = session_mgr.current_session.as_ref().unwrap();
            assert_eq!(session.tool_calls, 1);
        }

        // Test get_current_session
        let session_data = fs_tools.get_current_session();
        assert!(session_data.is_some());
        assert_eq!(session_data.unwrap().tool_calls, 1);

        // Test get_session_info
        let session_info = fs_tools.get_session_info();
        assert!(session_info.is_some());
        assert!(session_info.unwrap().contains("Tool calls: 1"));
    }

    #[test]
    fn test_fs_tools_update_session_with_lines_edited() {
        // Create a temporary directory for the test
        let temp_dir = tempdir().expect("Failed to create temp directory");
        let temp_path = temp_dir.path().to_path_buf();

        // Create a SessionStore using the temp directory
        let store = SessionStore::new(temp_path.join(".doge/sessions"))
            .expect("Failed to create session store");

        // Create a SessionManager
        let session_manager = Arc::new(Mutex::new(crate::session::SessionManager {
            store,
            current_session: None,
        }));

        // Create FsTools with session manager
        let repomap = Arc::new(RwLock::new(None));
        let fs_tools = FsTools::new(repomap).with_session_manager(session_manager.clone());

        // First create a session
        {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr
                .create_session()
                .expect("Failed to create session");
        }

        // Test update_session_with_lines_edited
        assert!(fs_tools.update_session_with_lines_edited(5).is_ok());

        // Check that the session was updated
        {
            let session_mgr = session_manager.lock().unwrap();
            let session = session_mgr.current_session.as_ref().unwrap();
            assert_eq!(session.lines_edited, 5);
        }
    }

    #[test]
    fn test_fs_tools_without_session_manager() {
        // Create FsTools without session manager
        let repomap = Arc::new(RwLock::new(None));
        let fs_tools = FsTools::new(repomap);

        // Test that session manager is None
        assert!(fs_tools.session_manager.is_none());

        // Test update_session_with_tool_call_count should not fail even without session manager
        assert!(fs_tools.update_session_with_tool_call_count().is_ok());

        // Test update_session_with_lines_edited should not fail even without session manager
        assert!(fs_tools.update_session_with_lines_edited(5).is_ok());

        // Test get_current_session should return None
        assert!(fs_tools.get_current_session().is_none());

        // Test get_session_info should return None
        assert!(fs_tools.get_session_info().is_none());
    }
}
