//! Rendering shared by subcommands.

use dgc_protocol::methods::DaemonStatus;

/// One compact JSON line.
pub fn daemon_status_json(status: &DaemonStatus) -> serde_json::Result<String> {
    serde_json::to_string(status)
}

pub fn daemon_status_text(status: &DaemonStatus) -> String {
    format!(
        "pid: {}\nprotocol_version: {}\nstarted_at: {}\nPATH: {}",
        status.pid, status.protocol_version, status.started_at, status.path_env
    )
}
