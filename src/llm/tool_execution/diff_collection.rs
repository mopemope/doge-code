//! Diff Collection Module
//!
//! This module handles the collection and processing of git diffs for the agent loop.
//! It provides functionality to collect diff information from tracked and untracked files,
//! and prepares the data for diff review.

use crate::diff_review::DiffReviewPayload;
use crate::execution::{
    ManagedProcessOutput, ManagedProcessSpec, ManagedProcessTermination, ManagedRunOptions,
    run_managed_process,
};
use crate::llm::LlmErrorKind;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

const GIT_DIFF_TIMEOUT: Duration = Duration::from_secs(30);

fn validate_git_output(
    allow_no_index_diff: bool,
    output: ManagedProcessOutput,
) -> Result<ManagedProcessOutput> {
    match output.termination {
        ManagedProcessTermination::TimedOut => {
            anyhow::bail!(
                "git command timed out after {} seconds",
                GIT_DIFF_TIMEOUT.as_secs()
            )
        }
        ManagedProcessTermination::Cancelled => {
            return Err(anyhow::anyhow!(LlmErrorKind::Cancelled));
        }
        ManagedProcessTermination::Exited => {}
    }

    if output.capture_truncated {
        anyhow::bail!("git output exceeded the capture limit; refusing incomplete diff review");
    }

    let accepted_exit =
        output.exit_code == Some(0) || (allow_no_index_diff && output.exit_code == Some(1));
    if !accepted_exit {
        let detail = output.stderr.trim();
        if detail.is_empty() {
            anyhow::bail!("git command failed with exit code {:?}", output.exit_code);
        }
        anyhow::bail!(
            "git command failed with exit code {:?}: {detail}",
            output.exit_code
        );
    }

    Ok(output)
}

async fn run_git_command(project_root: &Path, args: Vec<String>) -> Result<ManagedProcessOutput> {
    let allow_no_index_diff = args.iter().any(|arg| arg == "--no-index");
    let spec = ManagedProcessSpec {
        program: "git".to_string(),
        args,
        cwd: project_root.to_path_buf(),
        env: Default::default(),
    };
    let output = run_managed_process(spec, ManagedRunOptions::new(Some(GIT_DIFF_TIMEOUT))).await?;
    validate_git_output(allow_no_index_diff, output)
}

/// Collects diff review payload by examining git diffs and status.
///
/// When `filter_paths` is non-empty, the diff is scoped to only those
/// project-root-relative paths (the files the agent modified). This prevents
/// unrelated uncommitted work in the worktree from being included in the
/// review (and from being reverted on reject).
pub async fn collect_diff_review_payload(
    project_root: &Path,
    filter_paths: &[PathBuf],
) -> Result<Option<DiffReviewPayload>> {
    debug!(project_root = %project_root.display(), "Collecting diff review payload");

    let project_root: PathBuf = project_root.to_path_buf();

    // Spawn parallel tasks for git commands
    let tracked_diff_task = tokio::spawn({
        let project_root = project_root.clone();
        let filter_paths = filter_paths.to_vec();
        async move {
            let mut args = vec!["diff".to_string(), "--color=never".to_string()];
            if !filter_paths.is_empty() {
                args.push("--".to_string());
                args.extend(
                    filter_paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned()),
                );
            }
            run_git_command(&project_root, args).await
        }
    });

    let names_task = tokio::spawn({
        let project_root = project_root.clone();
        let filter_paths = filter_paths.to_vec();
        async move {
            let mut args = vec!["diff".to_string(), "--name-only".to_string()];
            if !filter_paths.is_empty() {
                args.push("--".to_string());
                args.extend(
                    filter_paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned()),
                );
            }
            run_git_command(&project_root, args).await
        }
    });

    let status_task = tokio::spawn({
        let project_root = project_root.clone();
        async move {
            run_git_command(
                &project_root,
                vec!["status".to_string(), "--porcelain=v1".to_string()],
            )
            .await
        }
    });

    // Wait for all tasks to complete
    let (tracked_diff, names_output, status_output) =
        tokio::join!(tracked_diff_task, names_task, status_task);

    // Process tracked diff
    let tracked_diff = tracked_diff??;
    let mut diff_sections = Vec::new();
    if !tracked_diff.stdout.is_empty() {
        diff_sections.push(tracked_diff.stdout);
    }

    // Process file names
    let names_output = names_output??;
    let mut files = names_output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    // Process status and untracked files
    let status_output = status_output??;
    let status_text = status_output.stdout;

    // Process untracked files in parallel
    let mut untracked_tasks = Vec::new();
    for line in status_text.lines() {
        let Some(path) = line.strip_prefix("?? ") else {
            continue;
        };

        if path.trim().is_empty() || path.ends_with('/') {
            continue;
        }

        let path = path.trim().to_string();

        // Scope to agent-modified files when a filter is provided
        if !filter_paths.is_empty()
            && !filter_paths.iter().any(|p| {
                let p_str = p.to_string_lossy();
                p_str.ends_with(path.as_str()) || path.ends_with(p_str.as_ref())
            })
        {
            continue;
        }
        let task = tokio::spawn({
            let project_root = project_root.clone();
            async move {
                let untracked_diff = run_git_command(
                    &project_root,
                    vec![
                        "diff".to_string(),
                        "--color=never".to_string(),
                        "--no-index".to_string(),
                        "/dev/null".to_string(),
                        path.clone(),
                    ],
                )
                .await
                .with_context(|| format!("failed to diff untracked file {path}"));
                (path, untracked_diff)
            }
        });
        untracked_tasks.push(task);
    }

    // Collect untracked file results
    for task in untracked_tasks {
        let result = task.await?;
        let untracked_diff = result.1?;
        let path = result.0;

        if !untracked_diff.stdout.is_empty() {
            diff_sections.push(untracked_diff.stdout);
        }

        if !files.contains(&path) {
            files.push(path);
        }
    }

    if diff_sections.is_empty() {
        debug!("No diffs detected");
        return Ok(None);
    }

    let mut combined_diff = diff_sections.join("\n");
    if !combined_diff.ends_with('\n') {
        combined_diff.push('\n');
    }

    debug!(files = ?files, "Collected diff review payload");

    Ok(Some(DiffReviewPayload {
        diff: combined_diff,
        files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_file(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    fn init_repo(dir: &Path) {
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["config", "user.email", "test@example.com"]);
        run_git(dir, &["config", "user.name", "Test User"]);
    }

    fn managed_output(
        termination: ManagedProcessTermination,
        exit_code: Option<i32>,
        capture_truncated: bool,
    ) -> ManagedProcessOutput {
        ManagedProcessOutput {
            termination,
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
            capture_truncated,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn test_validate_git_output_rejects_timeout_and_truncation() {
        let timeout = managed_output(ManagedProcessTermination::TimedOut, None, false);
        assert!(validate_git_output(false, timeout).is_err());

        let truncated = managed_output(ManagedProcessTermination::Exited, Some(0), true);
        assert!(validate_git_output(false, truncated).is_err());
    }

    #[test]
    fn test_validate_git_output_allows_no_index_difference_exit() {
        let output = managed_output(ManagedProcessTermination::Exited, Some(1), false);
        assert!(validate_git_output(true, output).is_ok());
        let output = managed_output(ManagedProcessTermination::Exited, Some(1), false);
        assert!(validate_git_output(false, output).is_err());
    }

    #[tokio::test]
    async fn test_collect_diff_review_payload_no_changes() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        init_repo(root);
        write_file(root, "foo.txt", "hello\n");
        run_git(root, &["add", "foo.txt"]);
        run_git(root, &["commit", "-q", "-m", "init"]);

        let payload = collect_diff_review_payload(root, &[]).await.unwrap();
        assert!(payload.is_none());
    }

    #[tokio::test]
    async fn test_collect_diff_review_payload_with_changes() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        init_repo(root);
        write_file(root, "foo.txt", "hello\n");
        run_git(root, &["add", "foo.txt"]);
        run_git(root, &["commit", "-q", "-m", "init"]);

        // tracked change
        write_file(root, "foo.txt", "hello world\n");
        // untracked change
        write_file(root, "bar.txt", "new file\n");

        let payload = collect_diff_review_payload(root, &[]).await.unwrap();
        let payload = payload.expect("expected diff payload");

        assert!(payload.files.iter().any(|f| f == "foo.txt"));
        assert!(payload.files.iter().any(|f| f == "bar.txt"));
        assert!(payload.diff.contains("foo.txt"));
        assert!(payload.diff.contains("bar.txt"));
    }

    #[tokio::test]
    async fn test_collect_diff_review_payload_filter_scopes_to_agent_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        init_repo(root);
        write_file(root, "agent_file.txt", "original\n");
        write_file(root, "user_file.txt", "user original\n");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "init"]);

        // Agent modifies one file; the user has unrelated uncommitted work in another
        write_file(root, "agent_file.txt", "agent modified\n");
        write_file(root, "user_file.txt", "user modified\n");

        let filter = vec![std::path::PathBuf::from("agent_file.txt")];
        let payload = collect_diff_review_payload(root, &filter)
            .await
            .unwrap()
            .expect("expected diff payload");

        assert!(payload.files.iter().any(|f| f == "agent_file.txt"));
        assert!(
            !payload.files.iter().any(|f| f == "user_file.txt"),
            "unrelated user changes must not be included in the review"
        );
        assert!(!payload.diff.contains("user_file.txt"));
    }
}
