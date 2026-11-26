//! End-to-end tests for `dgc project ...` and `dgc workspace ...`.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::{TestEnv, assert_success, stdout};
use serde_json::Value;

fn git(cwd: &Path, args: &[&str]) {
    // `tempdir()` lives directly under the temp dir; git must not look above
    // it (this host has a stray /tmp/.git).
    let ceiling = fs::canonicalize(std::env::temp_dir()).expect("temp dir");
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git should start");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json(output: &std::process::Output) -> Value {
    assert_success(output);
    serde_json::from_str(stdout(output).trim()).expect("stdout is JSON")
}

#[test]
fn project_and_worktree_workspace_lifecycle() {
    let env = TestEnv::new();
    let repo = fs::canonicalize(env.dir.path())
        .expect("canon")
        .join("repo");
    fs::create_dir(&repo).expect("mkdir");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);

    // A relative path is resolved against the CLI's working directory.
    let added = env
        .command(&["--json", "project", "add", "."])
        .current_dir(&repo)
        .output()
        .expect("dgc should start");
    let project = json(&added);
    assert_eq!(project["path"], repo.to_str().expect("utf8"));
    let project_id = project["id"].as_str().expect("id").to_owned();

    let listed = json(&env.run(&["--json", "project", "ls"]));
    assert_eq!(listed["projects"].as_array().expect("array").len(), 1);

    let created = json(&env.run(&[
        "--json",
        "workspace",
        "create",
        "--project",
        &project_id,
        "--worktree",
        "--branch",
        "feature/cli",
    ]));
    assert_eq!(created["isolation"], "worktree");
    assert_eq!(created["owned"], true);
    assert_eq!(created["branch"], "feature/cli");
    let ws_id = created["id"].as_str().expect("id").to_owned();
    let ws_path = Path::new(created["path"].as_str().expect("path")).to_owned();
    assert!(
        ws_path.starts_with(&env.paths.worktrees_dir),
        "{} must be under {}",
        ws_path.display(),
        env.paths.worktrees_dir.display()
    );
    assert!(ws_path.is_dir());

    let listed = json(&env.run(&["--json", "workspace", "ls", "--project", &project_id]));
    let items = listed["workspaces"].as_array().expect("array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], ws_id.as_str());

    // The project cannot be removed while the workspace is active.
    let refused = env.run(&["project", "rm", &project_id]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("active workspace"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let archived = json(&env.run(&["--json", "workspace", "archive", &ws_id]));
    assert_eq!(archived["state"], "archived");
    assert!(!ws_path.exists(), "owned worktree is removed");
    let listed = json(&env.run(&["--json", "workspace", "ls"]));
    assert!(listed["workspaces"].as_array().expect("array").is_empty());

    let removed = env.run(&["project", "rm", &project_id]);
    assert_success(&removed);
    assert!(
        repo.join(".git").exists(),
        "the repository itself is untouched"
    );
}

#[test]
fn text_output_and_local_workspace() {
    let env = TestEnv::new();
    let dir = fs::canonicalize(env.dir.path())
        .expect("canon")
        .join("plain");
    fs::create_dir(&dir).expect("mkdir");

    let added = env.run(&["project", "add", dir.to_str().expect("utf8")]);
    assert_success(&added);
    let line = stdout(&added);
    assert!(line.starts_with("prj_"), "{line}");
    assert!(line.contains("plain"), "{line}");
    let project_id = line.split_whitespace().next().expect("id").to_owned();

    let created = env.run(&["workspace", "create", "--project", &project_id]);
    assert_success(&created);
    let line = stdout(&created);
    assert!(line.starts_with("ws_"), "{line}");
    assert!(line.contains("local  active  -"), "{line}");
    let ws_id = line.split_whitespace().next().expect("id").to_owned();

    assert_success(&env.run(&["workspace", "archive", &ws_id]));
    assert!(dir.is_dir(), "a local workspace directory is never removed");
}

#[test]
fn worktree_requires_branch_and_unknown_ids_fail() {
    let env = TestEnv::new();
    let no_branch = env.run(&[
        "workspace",
        "create",
        "--project",
        "prj_00000000000000000000000000000000",
        "--worktree",
    ]);
    assert!(!no_branch.status.success(), "--worktree needs --branch");
    let unknown = env.run(&[
        "workspace",
        "archive",
        "ws_00000000000000000000000000000000",
    ]);
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown workspace"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
}
