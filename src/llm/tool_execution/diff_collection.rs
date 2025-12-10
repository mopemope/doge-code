//! Diff Collection Module
//!
//! This module handles the collection and processing of git diffs for the agent loop.
//! It provides functionality to collect diff information from tracked and untracked files,
//! and prepares the data for diff review.

use crate::diff_review::DiffReviewPayload;
use anyhow::{Context, Result};
use std::process::Command;
use tracing::debug;

/// Collects diff review payload by examining git diffs and status
pub async fn collect_diff_review_payload() -> Result<Option<DiffReviewPayload>> {
    debug!("Collecting diff review payload");

    // Spawn parallel tasks for git commands
    let tracked_diff_task = tokio::spawn(async {
        Command::new("git")
            .arg("diff")
            .arg("--color=never")
            .output()
            .context("failed to run git diff --color=never")
    });

    let names_task = tokio::spawn(async {
        Command::new("git")
            .arg("diff")
            .arg("--name-only")
            .output()
            .context("failed to run git diff --name-only")
    });

    let status_task = tokio::spawn(async {
        Command::new("git")
            .arg("status")
            .arg("--porcelain=v1")
            .output()
            .context("failed to run git status --porcelain")
    });

    // Wait for all tasks to complete
    let (tracked_diff, names_output, status_output) =
        tokio::join!(tracked_diff_task, names_task, status_task);

    // Process tracked diff
    let tracked_diff = tracked_diff??;
    let mut diff_sections = Vec::new();
    if !tracked_diff.stdout.is_empty() {
        let diff = String::from_utf8(tracked_diff.stdout)
            .context("git diff output was not valid UTF-8")?;
        diff_sections.push(diff);
    }

    // Process file names
    let names_output = names_output??;
    let mut files = String::from_utf8(names_output.stdout)
        .context("git diff --name-only output was not valid UTF-8")?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    // Process status and untracked files
    let status_output = status_output??;
    let status_text = String::from_utf8(status_output.stdout)
        .context("git status --porcelain output was not valid UTF-8")?;

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
        let task = tokio::spawn(async move {
            let untracked_diff = Command::new("git")
                .arg("diff")
                .arg("--color=never")
                .arg("--no-index")
                .arg("/dev/null")
                .arg(&path)
                .output()
                .with_context(|| format!("failed to diff untracked file {path}"));
            (path, untracked_diff)
        });
        untracked_tasks.push(task);
    }

    // Collect untracked file results
    for task in untracked_tasks {
        let result = task.await?;
        let untracked_diff = result.1?;
        let path = result.0;

        if !untracked_diff.stdout.is_empty() {
            let diff = String::from_utf8(untracked_diff.stdout)
                .context("git diff --no-index output for untracked file was not valid UTF-8")?;
            diff_sections.push(diff);
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

    #[test]
    fn test_collect_diff_review_payload_no_changes() {
        // Test when there are no changes
        // This would need mocking in a real test environment
    }

    #[test]
    fn test_collect_diff_review_payload_with_changes() {
        // Test when there are changes
        // This would need mocking in a real test environment
    }
}
