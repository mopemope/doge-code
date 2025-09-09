use crate::session::SessionManager;
use std::sync::{Arc, Mutex, OnceLock};

/// グローバルセッションマネージャーのインスタンスを保持する
static GLOBAL_SESSION_MANAGER: OnceLock<Arc<Mutex<SessionManager>>> = OnceLock::new();

/// グローバルセッションマネージャーを初期化する
pub fn init_global_session_manager(session_manager: SessionManager) {
    let _ = GLOBAL_SESSION_MANAGER.set(Arc::new(Mutex::new(session_manager)));
}

/// グローバルセッションマネージャーを取得する
pub fn get_global_session_manager() -> Option<Arc<Mutex<SessionManager>>> {
    GLOBAL_SESSION_MANAGER.get().cloned()
}