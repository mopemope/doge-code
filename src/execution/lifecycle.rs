//! Process lifecycle: process-group creation, graceful termination,
//! force kill, and child reaping.
//!
//! On Unix the spawned command becomes a process-group leader so that
//! timeout/cancellation kills the whole tree (child + grandchildren), not just
//! the direct child. Non-Unix platforms use a direct-child fallback; the
//! module boundary is kept so a Windows Job Object backend can be added later.

use std::time::Duration;
use tokio::process::Child;

/// Grace period between SIGTERM and SIGKILL for a process group.
pub const TERMINATE_GRACE_PERIOD: Duration = Duration::from_millis(500);

/// Configure a `tokio::process::Command` to start a new process group (Unix).
///
/// Must be called before `spawn`.
#[cfg(unix)]
pub fn configure_process_group(cmd: &mut tokio::process::Command) {
    // SAFETY: `setpgid(0, 0)` in the child between fork and exec is the
    // standard way to create a new process group. It only touches the child
    // process state and returns an error (rather than aborting) on failure.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// No-op on non-Unix: the fallback backend kills only the direct child.
#[cfg(not(unix))]
pub fn configure_process_group(_cmd: &mut tokio::process::Command) {}

/// Terminate a spawned child and its descendants, then reap the direct child.
///
/// Unix: SIGTERM the process group, wait a grace period, SIGKILL the group if
/// still alive, then `wait()` the direct child (reaping the zombie).
/// Fallback: `kill()` the direct child and `wait()` it.
pub async fn terminate_process_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        terminate_unix(child).await;
    }
    #[cfg(not(unix))]
    {
        terminate_fallback(child).await;
    }
}

#[cfg(unix)]
async fn terminate_unix(child: &mut Child) {
    let pid = match child.id() {
        Some(pid) => pid as libc::pid_t,
        None => {
            // No pid (already exited?): just reap.
            let _ = child.wait().await;
            return;
        }
    };
    // The child was spawned as a process-group leader, so its pgid == pid.
    unsafe {
        // Graceful: SIGTERM the group. ESRCH (no such process) means the tree
        // already exited; ignore errors here and fall through to reap.
        libc::killpg(pid, libc::SIGTERM);
    }
    // Wait for the direct child to exit within the grace period.
    let exited = tokio::time::timeout(TERMINATE_GRACE_PERIOD, child.wait())
        .await
        .is_ok();
    if !exited {
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
        // Reap after SIGKILL (blocking wait; the child must die now).
        let _ = child.wait().await;
    }
    // If the first wait succeeded via the timeout, the child is already
    // reaped (`wait()` consumes the exit). Nothing more to do.
}

#[cfg(not(unix))]
async fn terminate_fallback(child: &mut Child) {
    // Best effort: kill the direct child, then reap.
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Best-effort check whether a pid is still alive (Unix only).
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    unsafe {
        // Signal 0 performs no action but error reporting: 0 => alive,
        // ESRCH => gone, EPERM => alive but unkillable.
        let r = libc::kill(pid as libc::pid_t, 0);
        if r == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(unix)]
    async fn test_terminate_direct_child_is_reaped() {
        use std::process::Stdio;
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id().expect("pid");
        terminate_process_tree(&mut child).await;
        // Reaped: second wait returns immediately, process gone.
        assert!(!is_process_alive(pid), "child should be gone");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_terminate_kills_descendants() {
        use std::process::Stdio;
        // Parent bash spawns a long-lived grandchild and waits.
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c")
            .arg("sleep 30 & wait")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn bash");
        let parent_pid = child.id().expect("pid");
        // Give bash a moment to spawn `sleep`.
        tokio::time::sleep(Duration::from_millis(500)).await;
        terminate_process_tree(&mut child).await;
        assert!(!is_process_alive(parent_pid), "parent should be gone");
        // Descendant `sleep 30` must also be gone: no stray `sleep 30` from
        // our group should survive. Check via `pgrep -f` best-effort is flaky;
        // instead verify the process group is gone by signalling the pgid.
        let pgid_alive = unsafe { libc::killpg(parent_pid as libc::pid_t, 0) == 0 };
        assert!(!pgid_alive, "process group should be gone");
    }
}
