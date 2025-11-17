use crate::session::{SessionData, SessionManager};
use anyhow::Result;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct SessionManagerWrapper {
    session_manager: Option<Arc<Mutex<SessionManager>>>,
}

impl SessionManagerWrapper {
    pub fn new(session_manager: Option<Arc<Mutex<SessionManager>>>) -> Self {
        Self { session_manager }
    }

    /// Update the current session with tool call count
    pub fn update_session_with_tool_call_count(&self) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.update_current_session_with_tool_call_count()?;
        }
        Ok(())
    }

    /// Record a successful tool call in the current session
    pub fn record_tool_call_success(&self, tool_name: &str) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.record_tool_call_success(tool_name)?;
        }
        Ok(())
    }

    /// Record a failed tool call in the current session
    pub fn record_tool_call_failure(&self, tool_name: &str) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.record_tool_call_failure(tool_name)?;
        }
        Ok(())
    }

    /// Update the current session with lines edited count
    pub fn update_session_with_lines_edited(&self, lines_edited: u64) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            // Clone the store outside the mutable borrow scope
            let store = {
                let session_mgr = session_manager.lock().unwrap();
                session_mgr.store.clone()
            };

            // Update the session with lines edited
            {
                let mut session_mgr = session_manager.lock().unwrap();
                if let Some(ref mut session) = session_mgr.current_session {
                    session.increment_lines_edited(lines_edited);
                }
            }

            // Save the session
            if let Some(session_manager) = &self.session_manager {
                let session_mgr = session_manager.lock().unwrap();
                if let Some(ref session) = session_mgr.current_session {
                    store.save(session)?;
                }
            }
        }
        Ok(())
    }

    /// Update the current session with a changed file path
    pub fn update_session_with_changed_file(&self, path: std::path::PathBuf) -> Result<()> {
        if let Some(session_manager) = &self.session_manager {
            let mut session_mgr = session_manager.lock().unwrap();
            session_mgr.update_current_session_with_changed_file(path)?;
        }
        Ok(())
    }

    /// Get current session data
    pub fn get_current_session(&self) -> Option<SessionData> {
        if let Some(session_manager) = &self.session_manager {
            let session_mgr = session_manager.lock().unwrap();
            session_mgr.current_session.clone()
        } else {
            None
        }
    }

    /// Get reference to the session manager
    pub fn get_session_manager(&self) -> &Option<Arc<Mutex<SessionManager>>> {
        &self.session_manager
    }

    /// Get session info string
    pub fn get_session_info(&self) -> Option<String> {
        if let Some(session_manager) = &self.session_manager {
            let session_mgr = session_manager.lock().unwrap();
            (*session_mgr).current_session_info()
        } else {
            None
        }
    }
}
