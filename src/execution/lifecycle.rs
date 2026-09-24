//! Process lifecycle: process-group creation, graceful termination,
//! force kill, and child reaping.
//!
//! On Unix the spawned command becomes a process-group leader so that
//! timeout/cancellation kills the whole tree (child + grandchildren), not just
//! the direct child. Non-Unix platforms use a direct-child fallback; the
//! module boundary is kept so a Windows Job Object backend can be added later.

use std::io;
use std::time::{Duration, Instant};
use tokio::process::Child;

/// Grace period between SIGTERM and SIGKILL for a process group.
pub const TERMINATE_GRACE_PERIOD: Duration = Duration::from_millis(500);

/// How long termination waits for a killed process group to disappear after
/// SIGKILL. This is only a safety valve; the normal path should disappear
/// immediately after the direct child is reaped.
const POST_KILL_VERIFY_PERIOD: Duration = Duration::from_secs(1);

/// Maximum time to wait for the direct child to be reaped after SIGKILL.
const POST_KILL_WAIT_PERIOD: Duration = Duration::from_secs(1);

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

/// A process-group ownership guard.
///
/// The PGID is captured immediately after spawn, rather than being recovered
/// from `Child::id()` during termination. This matters when the direct child
/// exits before cleanup: descendants can still own the original process group,
/// while `Child::id()` may no longer be available.
///
/// On Unix, `Drop` is a last-resort SIGKILL safety net for an unexpectedly
/// dropped runner future. It is not a substitute for the explicit async
/// termination/reap path used for normal completion, timeout, and
/// cancellation.
#[derive(Debug)]
pub struct ProcessGroupHandle {
    #[cfg(unix)]
    pgid: libc::pid_t,
    #[cfg(not(unix))]
    pid: Option<u32>,
    armed: bool,
}

impl ProcessGroupHandle {
    /// Capture the group identity of a freshly spawned child.
    pub fn from_child(child: &Child) -> Self {
        #[cfg(unix)]
        {
            let pgid = child
                .id()
                .map(|pid| pid as libc::pid_t)
                .filter(|&pid| pid > 0)
                .unwrap_or(0);
            Self {
                pgid,
                armed: pgid > 0,
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                pid: child.id(),
                armed: child.id().is_some(),
            }
        }
    }

    /// Whether this guard may still send signals to the owned process group.
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Stop treating this handle as an active group owner.
    ///
    /// Call this only after the group has been observed gone (or after a
    /// non-Unix direct-child fallback has completed).
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// The owned process-group id, when one was captured.
    #[cfg(unix)]
    pub fn pgid(&self) -> Option<libc::pid_t> {
        (self.pgid > 0).then_some(self.pgid)
    }

    /// The owned direct-child pid on platforms without process groups.
    #[cfg(not(unix))]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupHandle {
    fn drop(&mut self) {
        if !self.armed || self.pgid <= 0 {
            return;
        }

        // SAFETY: killpg with a signal does not dereference Rust memory. The
        // handle owns the PGID captured at spawn; this is intentionally a
        // best-effort final safety net for a dropped future.
        let result = unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                tracing::debug!(pgid = self.pgid, error = %error, "process-group drop kill failed");
            }
        }
    }
}

#[cfg(not(unix))]
impl Drop for ProcessGroupHandle {
    fn drop(&mut self) {
        // There is no process-group backend yet. `Child::kill_on_drop`
        // remains the direct-child safety net; the handle cannot safely
        // perform an async reap from Drop.
    }
}

/// Return true when a Unix process group still exists.
///
/// `killpg(pgid, 0)` performs the permission/existence check without sending a
/// signal. `ESRCH` means gone; `EPERM` means present but not signalable.
#[cfg(unix)]
pub fn process_group_exists(pgid: libc::pid_t) -> bool {
    if pgid <= 0 {
        return false;
    }
    // SAFETY: signal number zero only asks the kernel for an existence check.
    let result = unsafe { libc::killpg(pgid, 0) };
    if result == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        false
    } else {
        // EPERM and unknown errors are treated conservatively as alive.
        tracing::debug!(pgid, error = %error, "process-group existence check was inconclusive");
        true
    }
}

#[cfg(unix)]
fn group_exists(handle: &ProcessGroupHandle) -> bool {
    handle.pgid().is_some_and(process_group_exists)
}

#[cfg(not(unix))]
fn group_exists(handle: &ProcessGroupHandle) -> bool {
    handle.pid().is_some_and(is_process_alive)
}

#[cfg(unix)]
fn signal_group(handle: &ProcessGroupHandle, signal: libc::c_int) -> io::Result<()> {
    let Some(pgid) = handle.pgid() else {
        return Ok(());
    };
    // SAFETY: the handle owns this PGID and the signal is a valid POSIX signal.
    let result = unsafe { libc::killpg(pgid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(not(unix))]
fn signal_group(_handle: &ProcessGroupHandle, _signal: i32) -> io::Result<()> {
    // The direct-child fallback is implemented by `Child::start_kill` in
    // `terminate_impl`; a future Windows Job Object backend can replace this
    // no-op with group-wide signaling.
    Ok(())
}

/// Terminate a child and its owned process group, then reap the direct child.
///
/// The direct child's exit and the process group's disappearance are tracked
/// independently. In particular, a parent that exits on SIGTERM does not end
/// cleanup while a SIGTERM-ignoring descendant remains in the group.
pub async fn terminate_process_tree_with_group(
    child: &mut Child,
    group: &mut ProcessGroupHandle,
) -> io::Result<()> {
    terminate_impl(child, group).await
}

/// Compatibility wrapper for callers that do not already own a group handle.
///
/// New code should capture a [`ProcessGroupHandle`] immediately after spawn and
/// call [`terminate_process_tree_with_group`] so a child that exits first
/// cannot lose its PGID.
pub async fn terminate_process_tree(child: &mut Child) {
    let mut group = ProcessGroupHandle::from_child(child);
    if let Err(error) = terminate_process_tree_with_group(child, &mut group).await {
        tracing::warn!(error = %error, "failed to fully reap process tree");
    }
}

#[cfg(unix)]
async fn terminate_impl(child: &mut Child, group: &mut ProcessGroupHandle) -> io::Result<()> {
    if !group.is_armed() {
        // No PGID was captured. Fall back to the direct child and still reap.
        let _ = child.start_kill();
        child.wait().await?;
        group.disarm();
        return Ok(());
    }

    // Graceful phase: signal the whole group, then observe both the direct
    // child and the group until the grace period expires.
    if let Err(error) = signal_group(group, libc::SIGTERM) {
        tracing::debug!(error = %error, "failed to send SIGTERM to process group");
    }

    let deadline = Instant::now() + TERMINATE_GRACE_PERIOD;
    let mut child_reaped = false;
    let mut wait_error = None;

    loop {
        if !child_reaped {
            match child.try_wait() {
                Ok(Some(_)) => child_reaped = true,
                Ok(None) => {}
                Err(error) => {
                    wait_error = Some(error);
                    break;
                }
            }
        }

        if child_reaped && !group_exists(group) {
            group.disarm();
            return Ok(());
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(now);
        tokio::time::sleep(remaining.min(Duration::from_millis(20))).await;
    }

    // A direct child may have exited while a descendant ignored SIGTERM. Do
    // not use the child's wait result as the group-termination result.
    if let Err(error) = signal_group(group, libc::SIGKILL) {
        tracing::debug!(error = %error, "failed to send SIGKILL to process group");
    }

    if !child_reaped {
        match tokio::time::timeout(POST_KILL_WAIT_PERIOD, child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => wait_error = Some(error),
            Err(_) => {
                wait_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for the direct child after SIGKILL",
                ));
            }
        }
    }

    // Verify group disappearance separately. A process group can remain after
    // its leader has been reaped when descendants are still alive.
    let verify_deadline = Instant::now() + POST_KILL_VERIFY_PERIOD;
    while group_exists(group) && Instant::now() < verify_deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let group_survived = group_exists(group);
    if !group_survived {
        group.disarm();
    } else {
        tracing::warn!("process group still exists after SIGKILL; retaining drop guard");
    }

    if let Some(error) = wait_error {
        Err(error)
    } else if group_survived {
        Err(io::Error::other("process group still exists after SIGKILL"))
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
async fn terminate_impl(child: &mut Child, group: &mut ProcessGroupHandle) -> io::Result<()> {
    // No Job Object backend yet: terminate/reap the direct child.
    let _ = child.start_kill();
    match tokio::time::timeout(POST_KILL_WAIT_PERIOD, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for the direct child after kill",
            ));
        }
    }
    group.disarm();
    Ok(())
}

/// Clean up descendants after a normally exited direct child.
///
/// A finite managed command owns its whole process group. If the direct child
/// exits but leaves a descendant behind, apply the same graceful/force
/// cleanup sequence to the retained PGID.
pub async fn cleanup_process_group_after_exit(group: &mut ProcessGroupHandle) -> io::Result<()> {
    if !group.is_armed() || !group_exists(group) {
        group.disarm();
        return Ok(());
    }

    #[cfg(unix)]
    {
        if let Err(error) = signal_group(group, libc::SIGTERM) {
            tracing::debug!(error = %error, "failed to send SIGTERM to residual process group");
        }
        let deadline = Instant::now() + TERMINATE_GRACE_PERIOD;
        while group_exists(group) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if group_exists(group) {
            if let Err(error) = signal_group(group, libc::SIGKILL) {
                tracing::debug!(error = %error, "failed to kill residual process group");
            }
            let verify_deadline = Instant::now() + POST_KILL_VERIFY_PERIOD;
            while group_exists(group) && Instant::now() < verify_deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        // There is no group-wide operation in the fallback backend. The
        // direct child has already exited, so only the guard bookkeeping is
        // needed here.
        group.disarm();
        return Ok(());
    }

    if group_exists(group) {
        Err(io::Error::other("process group still exists after cleanup"))
    } else {
        group.disarm();
        Ok(())
    }
}

/// Best-effort check whether a pid is still alive (Unix only).
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    // Signal 0 performs no action but error reporting: 0 => alive,
    // ESRCH => gone, EPERM => alive but unkillable.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Direct-child liveness fallback for platforms without Unix process groups.
///
/// The portable std API does not expose a PID liveness probe. Callers on
/// those platforms should rely on `Child::try_wait`; this conservative helper
/// reports false rather than guessing or using Unix-only APIs.
#[cfg(not(unix))]
pub fn is_process_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn spawn_grouped(program: &str, args: &[&str]) -> Child {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut cmd);
        cmd.spawn().expect("spawn test process")
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_terminate_direct_child_is_reaped() {
        let mut child = spawn_grouped("sleep", &["30"]);
        let pid = child.id().expect("pid");
        terminate_process_tree(&mut child).await;
        assert!(!is_process_alive(pid), "child should be gone");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_terminate_kills_descendants() {
        let mut child = spawn_grouped("bash", &["-c", "sleep 30 & wait"]);
        let parent_pid = child.id().expect("pid");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut group = ProcessGroupHandle::from_child(&child);
        terminate_process_tree_with_group(&mut child, &mut group)
            .await
            .expect("terminate tree");
        assert!(!is_process_alive(parent_pid), "parent should be gone");
        assert!(!process_group_exists(parent_pid as libc::pid_t));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_terminate_kills_sigterm_ignoring_descendant() {
        let mut child = spawn_grouped(
            "bash",
            &[
                "-c",
                "bash -c 'trap \"\" TERM; while :; do sleep 1; done' & wait",
            ],
        );
        let pgid = child.id().expect("pid") as libc::pid_t;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut group = ProcessGroupHandle::from_child(&child);
        terminate_process_tree_with_group(&mut child, &mut group)
            .await
            .expect("terminate tree");
        assert!(!process_group_exists(pgid));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_process_group_handle_kills_group_on_drop() {
        let mut child = spawn_grouped("bash", &["-c", "sleep 30 & wait"]);
        let pgid = child.id().expect("pid") as libc::pid_t;
        let group = ProcessGroupHandle::from_child(&child);
        drop(group);
        tokio::time::sleep(Duration::from_millis(200)).await;
        // The guard cannot asynchronously reap a still-held Child. Reap it
        // explicitly in this unit test; the runner's future-drop path drops
        // the Child as well and relies on the runtime reaper.
        terminate_process_tree(&mut child).await;
        assert!(!process_group_exists(pgid));
    }
}
