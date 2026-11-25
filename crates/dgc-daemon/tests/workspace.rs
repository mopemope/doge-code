//! WorkspaceManager against real git repositories in temp directories.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dgc_daemon::store::Store;
use dgc_daemon::workspace::WorkspaceManager;
use dgc_protocol::ids::{ProjectId, WorkspaceId};
use dgc_protocol::methods::WorkspaceCreateParams;
use dgc_protocol::model::{Isolation, Workspace, WorkspaceState};
use dgc_protocol::rpc::{INTERNAL, INVALID_PARAMS, INVALID_STATE, NOT_FOUND};
use tempfile::{TempDir, tempdir};

/// Run git with a hermetic environment; panics on failure. Returns stdout.
fn git(cwd: &Path, args: &[&str]) -> String {
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
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_owned()
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    !git(repo, &["branch", "--list", branch]).is_empty()
}

struct Env {
    _dir: TempDir,
    root: PathBuf,
    repo: PathBuf,
    worktrees: PathBuf,
    store: Arc<Store>,
    manager: WorkspaceManager,
}

impl Env {
    fn new() -> Self {
        let dir = tempdir().expect("tempdir");
        let root = fs::canonicalize(dir.path()).expect("canonicalize");
        let repo = root.join("base/repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("a.txt"), "one\n").expect("write");
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let worktrees = root.join("data/worktrees");
        let store = Arc::new(Store::open(&root.join("state")).expect("store"));
        let manager = WorkspaceManager::new(Arc::clone(&store), worktrees.clone());
        Self {
            _dir: dir,
            root,
            repo,
            worktrees,
            store,
            manager,
        }
    }

    async fn project(&self) -> dgc_protocol::model::Project {
        self.manager.add_project(&self.repo).await.expect("add")
    }

    fn worktree_params(project: &ProjectId, branch: &str) -> WorkspaceCreateParams {
        WorkspaceCreateParams {
            project_id: project.clone(),
            isolation: Isolation::Worktree,
            branch: Some(branch.to_owned()),
            base_ref: None,
            slug: None,
        }
    }

    async fn worktree(&self, branch: &str) -> (ProjectId, Workspace) {
        let project = self.project().await;
        let ws = self
            .manager
            .create_workspace(Self::worktree_params(&project.id, branch))
            .await
            .expect("create worktree");
        (project.id, ws)
    }
}

#[tokio::test]
async fn add_project_is_idempotent() {
    let env = Env::new();
    let first = env.project().await;
    // Same directory spelled differently resolves to the same project.
    let spelled = env.repo.join("../repo/.");
    let second = env.manager.add_project(&spelled).await.expect("add again");
    assert_eq!(first, second);
    assert_eq!(first.path, env.repo);
    assert_eq!(first.name, "repo");
    assert_eq!(env.manager.list_projects().await.expect("list").len(), 1);
}

#[tokio::test]
async fn add_project_requires_an_existing_directory() {
    let env = Env::new();
    let missing = env.manager.add_project(&env.root.join("nope")).await;
    assert_eq!(missing.expect_err("missing").code(), INVALID_PARAMS);
    let file = env.repo.join("a.txt");
    let not_dir = env.manager.add_project(&file).await;
    assert_eq!(not_dir.expect_err("file").code(), INVALID_PARAMS);
}

#[tokio::test]
async fn local_workspace_is_not_owned() {
    let env = Env::new();
    let project = env.project().await;
    let ws = env
        .manager
        .create_workspace(WorkspaceCreateParams {
            project_id: project.id.clone(),
            isolation: Isolation::Local,
            branch: None,
            base_ref: None,
            slug: None,
        })
        .await
        .expect("local workspace");
    assert_eq!(ws.isolation, Isolation::Local);
    assert_eq!(ws.path, project.path);
    assert!(!ws.owned);
    assert_eq!(ws.state, WorkspaceState::Active);
    assert_eq!(ws.branch, None);
    let stored = env.store.get_workspace(&ws.id).expect("get").expect("some");
    assert_eq!(stored, ws);
}

#[tokio::test]
async fn local_workspace_rejects_worktree_only_fields() {
    let env = Env::new();
    let project = env.project().await;
    let err = env
        .manager
        .create_workspace(WorkspaceCreateParams {
            project_id: project.id,
            isolation: Isolation::Local,
            branch: Some("x".into()),
            base_ref: None,
            slug: None,
        })
        .await
        .expect_err("branch on a local workspace");
    assert_eq!(err.code(), INVALID_PARAMS);
}

#[tokio::test]
async fn workspace_for_unknown_project_is_not_found() {
    let env = Env::new();
    let err = env
        .manager
        .create_workspace(Env::worktree_params(&ProjectId::generate(), "x"))
        .await
        .expect_err("unknown project");
    assert_eq!(err.code(), NOT_FOUND);
}

#[tokio::test]
async fn worktree_created_under_managed_dir() {
    let env = Env::new();
    let (project_id, ws) = env.worktree("feature/login").await;
    assert_eq!(ws.isolation, Isolation::Worktree);
    assert!(ws.owned);
    assert_eq!(ws.branch.as_deref(), Some("feature/login"));
    assert_eq!(ws.state, WorkspaceState::Active);

    // <worktrees>/<repo>-<12 uuid chars>/<slug>
    let relative = ws
        .path
        .strip_prefix(&env.worktrees)
        .expect("under worktrees");
    let parts: Vec<_> = relative.iter().map(|p| p.to_string_lossy()).collect();
    assert_eq!(parts.len(), 2, "{parts:?}");
    let uuid_part = &project_id.as_str()["prj_".len()..];
    assert_eq!(
        parts[0],
        format!("repo-{}", &uuid_part[uuid_part.len() - 12..])
    );
    assert_eq!(parts[1], "feature_login");

    assert!(ws.path.join("a.txt").is_file());
    assert_eq!(
        git(&ws.path, &["branch", "--show-current"]),
        "feature/login"
    );
    let head = git(&env.repo, &["rev-parse", "HEAD"]);
    assert_eq!(ws.base_ref.as_deref(), Some(head.as_str()));
    assert_eq!(git(&ws.path, &["rev-parse", "HEAD"]), head);

    let listed = env.manager.list_workspaces(None).await.expect("list");
    assert_eq!(listed, vec![ws]);
}

#[tokio::test]
async fn worktree_uses_explicit_slug_and_base_ref() {
    let env = Env::new();
    let first = git(&env.repo, &["rev-parse", "HEAD"]);
    fs::write(env.repo.join("a.txt"), "two\n").expect("write");
    git(&env.repo, &["commit", "-q", "-am", "second"]);
    git(&env.repo, &["tag", "v1", &first]);

    let project = env.project().await;
    let ws = env
        .manager
        .create_workspace(WorkspaceCreateParams {
            project_id: project.id,
            isolation: Isolation::Worktree,
            branch: Some("fix".into()),
            base_ref: Some("v1".into()),
            slug: Some("my-slug_1.0".into()),
        })
        .await
        .expect("create");
    assert_eq!(ws.path.file_name().expect("name"), "my-slug_1.0");
    assert_eq!(ws.base_ref.as_deref(), Some(first.as_str()));
    assert_eq!(
        fs::read_to_string(ws.path.join("a.txt")).expect("read"),
        "one\n"
    );
}

#[tokio::test]
async fn existing_branch_is_rejected() {
    let env = Env::new();
    git(&env.repo, &["branch", "taken"]);
    let project = env.project().await;
    let err = env
        .manager
        .create_workspace(Env::worktree_params(&project.id, "taken"))
        .await
        .expect_err("existing branch");
    assert_eq!(err.code(), INVALID_PARAMS);
    assert!(
        env.manager
            .list_workspaces(None)
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn worktree_input_is_validated() {
    let env = Env::new();
    let project = env.project().await;
    let cases: Vec<(&str, WorkspaceCreateParams)> = vec![
        ("missing branch", {
            let mut p = Env::worktree_params(&project.id, "x");
            p.branch = None;
            p
        }),
        (
            "option-like branch",
            Env::worktree_params(&project.id, "-D"),
        ),
        (
            "invalid ref name",
            Env::worktree_params(&project.id, "a..b"),
        ),
        ("empty branch", Env::worktree_params(&project.id, "")),
        ("unknown base ref", {
            let mut p = Env::worktree_params(&project.id, "ok1");
            p.base_ref = Some("no-such-ref".into());
            p
        }),
        ("option-like base ref", {
            let mut p = Env::worktree_params(&project.id, "ok2");
            p.base_ref = Some("--all".into());
            p
        }),
        ("traversal slug", {
            let mut p = Env::worktree_params(&project.id, "ok3");
            p.slug = Some("..".into());
            p
        }),
        ("slash slug", {
            let mut p = Env::worktree_params(&project.id, "ok4");
            p.slug = Some("a/b".into());
            p
        }),
        ("dot slug", {
            let mut p = Env::worktree_params(&project.id, "ok5");
            p.slug = Some(".hidden".into());
            p
        }),
        ("empty slug", {
            let mut p = Env::worktree_params(&project.id, "ok6");
            p.slug = Some(String::new());
            p
        }),
    ];
    for (label, params) in cases {
        let err = env.manager.create_workspace(params).await.expect_err(label);
        assert_eq!(err.code(), INVALID_PARAMS, "{label}: {}", err.message());
    }
    assert_eq!(
        git(&env.repo, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1,
        "rejected requests must not leave worktrees behind"
    );
    assert!(git(&env.repo, &["branch", "--list", "ok*"]).is_empty());
}

#[tokio::test]
async fn worktree_path_collision_is_rejected() {
    let env = Env::new();
    let (project_id, _first) = env.worktree("a/b").await;
    // Different branch, same derived slug `a_b`.
    let err = env
        .manager
        .create_workspace(Env::worktree_params(&project_id, "a_b"))
        .await
        .expect_err("slug collision");
    assert_eq!(err.code(), INVALID_PARAMS);
    assert!(!branch_exists(&env.repo, "a_b"));
}

#[tokio::test]
async fn worktree_requires_a_git_repository() {
    let env = Env::new();
    let plain = env.root.join("base/plain");
    fs::create_dir_all(&plain).expect("mkdir");
    let project = env.manager.add_project(&plain).await.expect("add");
    let err = env
        .manager
        .create_workspace(Env::worktree_params(&project.id, "x"))
        .await
        .expect_err("not a repo");
    assert_eq!(err.code(), INVALID_PARAMS);
    assert!(
        err.message().contains("not a git repository"),
        "{}",
        err.message()
    );
}

#[tokio::test]
async fn worktree_rejects_repository_subdirectory() {
    let env = Env::new();
    let sub = env.repo.join("sub");
    fs::create_dir(&sub).expect("mkdir");
    let project = env.manager.add_project(&sub).await.expect("add");
    let err = env
        .manager
        .create_workspace(Env::worktree_params(&project.id, "x"))
        .await
        .expect_err("subdirectory");
    assert_eq!(err.code(), INVALID_PARAMS);
}

#[tokio::test]
async fn archive_removes_owned_worktree() {
    let env = Env::new();
    let (_project, ws) = env.worktree("feature/x").await;
    assert!(ws.path.exists());
    assert!(branch_exists(&env.repo, "feature/x"));

    let archived = env
        .manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert_eq!(archived.state, WorkspaceState::Archived);
    assert_eq!(archived.id, ws.id);
    assert!(!ws.path.exists(), "worktree directory removed");
    assert!(!branch_exists(&env.repo, "feature/x"), "branch removed");
    assert!(
        !git(&env.repo, &["worktree", "list", "--porcelain"]).contains("feature/x"),
        "worktree registration removed"
    );
    let stored = env.store.get_workspace(&ws.id).expect("get").expect("some");
    assert_eq!(stored.state, WorkspaceState::Archived);
    assert!(
        env.manager
            .list_workspaces(None)
            .await
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn archive_removes_dirty_worktree() {
    let env = Env::new();
    let (_project, ws) = env.worktree("dirty").await;
    fs::write(ws.path.join("untracked.txt"), "x").expect("write");
    fs::write(ws.path.join("a.txt"), "changed\n").expect("write");
    env.manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert!(!ws.path.exists());
}

#[tokio::test]
async fn archive_is_idempotent() {
    let env = Env::new();
    let (_project, ws) = env.worktree("twice").await;
    let first = env.manager.archive_workspace(&ws.id).await.expect("first");
    let second = env.manager.archive_workspace(&ws.id).await.expect("second");
    assert_eq!(first, second);
}

#[tokio::test]
async fn archive_unknown_workspace_is_not_found() {
    let env = Env::new();
    let err = env
        .manager
        .archive_workspace(&WorkspaceId::generate())
        .await
        .expect_err("unknown");
    assert_eq!(err.code(), NOT_FOUND);
}

#[tokio::test]
async fn archive_keeps_local_directory() {
    let env = Env::new();
    let project = env.project().await;
    let ws = env
        .manager
        .create_workspace(WorkspaceCreateParams {
            project_id: project.id,
            isolation: Isolation::Local,
            branch: None,
            base_ref: None,
            slug: None,
        })
        .await
        .expect("local");
    let archived = env
        .manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert_eq!(archived.state, WorkspaceState::Archived);
    assert!(
        env.repo.join("a.txt").is_file(),
        "project directory untouched"
    );
}

#[tokio::test]
async fn archive_refuses_path_outside_managed_dir() {
    let env = Env::new();
    let (_project, mut ws) = env.worktree("guarded").await;
    let real_worktree = ws.path.clone();
    let victim = env.root.join("victim");
    fs::create_dir_all(&victim).expect("mkdir");
    fs::write(victim.join("precious.txt"), "keep").expect("write");

    // A tampered record: owned, but pointing outside the managed directory.
    ws.path = victim.clone();
    env.store.put_workspace(&ws).expect("put tampered");

    let archived = env
        .manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert_eq!(archived.state, WorkspaceState::Archived);
    assert!(
        victim.join("precious.txt").is_file(),
        "outside directory kept"
    );
    assert!(real_worktree.exists(), "nothing else was removed");
    assert!(branch_exists(&env.repo, "guarded"), "branch kept");
}

#[tokio::test]
async fn archive_refuses_traversal_path() {
    let env = Env::new();
    let (_project, mut ws) = env.worktree("trav").await;
    let victim = env.root.join("victim");
    fs::create_dir_all(&victim).expect("mkdir");
    fs::write(victim.join("precious.txt"), "keep").expect("write");
    // Lexically starts with the managed dir, but `..` escapes it.
    ws.path = env.worktrees.join("../../victim");
    env.store.put_workspace(&ws).expect("put tampered");

    env.manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert!(victim.join("precious.txt").is_file());
    assert!(branch_exists(&env.repo, "trav"));
}

#[tokio::test]
async fn archive_refuses_symlink_escape() {
    let env = Env::new();
    let (_project, mut ws) = env.worktree("link").await;
    let victim = env.root.join("victim");
    fs::create_dir_all(&victim).expect("mkdir");
    fs::write(victim.join("precious.txt"), "keep").expect("write");
    let link = ws.path.parent().expect("parent").join("escape");
    symlink(&victim, &link).expect("symlink");
    ws.path = link;
    env.store.put_workspace(&ws).expect("put tampered");

    env.manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert!(victim.join("precious.txt").is_file());
    assert!(branch_exists(&env.repo, "link"));
}

#[tokio::test]
async fn archive_keeps_unowned_directory_inside_managed_dir() {
    let env = Env::new();
    let (_project, mut ws) = env.worktree("unowned").await;
    ws.owned = false;
    env.store.put_workspace(&ws).expect("put");
    env.manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert!(ws.path.exists(), "unowned directory kept");
    assert!(branch_exists(&env.repo, "unowned"), "unowned branch kept");
}

#[tokio::test]
async fn archive_failure_leaves_workspace_active() {
    let env = Env::new();
    let (_project, mut ws) = env.worktree("fails").await;
    // Inside the managed dir but not a registered worktree: git refuses.
    let fake = ws.path.parent().expect("parent").join("fake");
    fs::create_dir_all(&fake).expect("mkdir");
    fs::write(fake.join("note.txt"), "keep").expect("write");
    ws.path = fake.clone();
    env.store.put_workspace(&ws).expect("put");

    let err = env
        .manager
        .archive_workspace(&ws.id)
        .await
        .expect_err("git removal fails");
    assert_eq!(err.code(), INTERNAL);
    assert!(err.message().len() <= 4096 + 256, "bounded message");
    let stored = env.store.get_workspace(&ws.id).expect("get").expect("some");
    assert_eq!(stored.state, WorkspaceState::Active);
    assert!(fake.join("note.txt").is_file());
    assert!(branch_exists(&env.repo, "fails"));
}

#[tokio::test]
async fn archive_tolerates_a_worktree_directory_that_is_already_gone() {
    let env = Env::new();
    let (_project, ws) = env.worktree("gone").await;
    // An unrelated worktree of the user whose directory is also missing: it
    // must keep its registration (no repository-wide prune).
    let user_wt = env.root.join("base/user-wt");
    git(
        &env.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "user-branch",
            user_wt.to_str().expect("utf8"),
        ],
    );
    fs::remove_dir_all(&user_wt).expect("rm user worktree");
    fs::remove_dir_all(&ws.path).expect("rm");

    let archived = env
        .manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert_eq!(archived.state, WorkspaceState::Archived);
    assert!(
        !branch_exists(&env.repo, "gone"),
        "dgc's own branch removed"
    );
    let registered = git(&env.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        registered.contains(user_wt.to_str().expect("utf8")),
        "the user's stale worktree must stay registered:\n{registered}"
    );
    assert!(
        !registered.contains(ws.path.to_str().expect("utf8")),
        "this workspace's registration is gone"
    );
    assert!(branch_exists(&env.repo, "user-branch"));
}

#[tokio::test]
async fn archive_keeps_a_branch_the_user_recreated_after_the_registration_was_gone() {
    let env = Env::new();
    let (_project, ws) = env.worktree("feature/x").await;
    // The user removes dgc's worktree and branch by hand, then reuses the name.
    fs::remove_dir_all(&ws.path).expect("rm");
    git(&env.repo, &["worktree", "prune"]);
    git(&env.repo, &["branch", "-D", "feature/x"]);
    fs::write(env.repo.join("a.txt"), "user work\n").expect("write");
    git(&env.repo, &["commit", "-q", "-am", "user commit"]);
    git(&env.repo, &["branch", "feature/x"]);
    let tip = git(&env.repo, &["rev-parse", "feature/x"]);

    let archived = env
        .manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert_eq!(archived.state, WorkspaceState::Archived);
    assert!(
        branch_exists(&env.repo, "feature/x"),
        "the user's branch survives"
    );
    assert_eq!(git(&env.repo, &["rev-parse", "feature/x"]), tip);
}

#[tokio::test]
async fn archive_keeps_both_branches_when_the_user_switched_branch_in_the_worktree() {
    let env = Env::new();
    let (_project, ws) = env.worktree("orig").await;
    git(&ws.path, &["switch", "-q", "-c", "switched"]);

    let archived = env
        .manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert_eq!(archived.state, WorkspaceState::Archived);
    assert!(!ws.path.exists(), "dgc's own worktree directory is removed");
    assert!(branch_exists(&env.repo, "orig"), "recorded branch kept");
    assert!(branch_exists(&env.repo, "switched"), "user's branch kept");
}

#[tokio::test]
async fn archive_keeps_the_branch_when_the_worktree_is_detached() {
    let env = Env::new();
    let (_project, ws) = env.worktree("detach").await;
    git(&ws.path, &["checkout", "-q", "--detach"]);

    env.manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    assert!(!ws.path.exists());
    assert!(branch_exists(&env.repo, "detach"));
}

#[tokio::test]
async fn remove_project_with_active_workspace_fails() {
    let env = Env::new();
    let (project_id, ws) = env.worktree("busy").await;
    let err = env
        .manager
        .remove_project(&project_id)
        .await
        .expect_err("active workspace");
    assert_eq!(err.code(), INVALID_STATE);
    assert_eq!(env.manager.list_projects().await.expect("list").len(), 1);

    env.manager
        .archive_workspace(&ws.id)
        .await
        .expect("archive");
    env.manager
        .remove_project(&project_id)
        .await
        .expect("remove");
    assert!(env.manager.list_projects().await.expect("list").is_empty());
    assert!(env.repo.join("a.txt").is_file(), "repository untouched");
}

#[tokio::test]
async fn remove_unknown_project_is_not_found() {
    let env = Env::new();
    let err = env
        .manager
        .remove_project(&ProjectId::generate())
        .await
        .expect_err("unknown");
    assert_eq!(err.code(), NOT_FOUND);
}

#[tokio::test]
async fn list_workspaces_filters_by_project() {
    let env = Env::new();
    let other = env.root.join("base/other");
    fs::create_dir_all(&other).expect("mkdir");
    let (first_project, ws) = env.worktree("one").await;
    let second = env.manager.add_project(&other).await.expect("add");
    let local = env
        .manager
        .create_workspace(WorkspaceCreateParams {
            project_id: second.id.clone(),
            isolation: Isolation::Local,
            branch: None,
            base_ref: None,
            slug: None,
        })
        .await
        .expect("local");
    let all = env.manager.list_workspaces(None).await.expect("all");
    assert_eq!(all.len(), 2);
    let only_first = env
        .manager
        .list_workspaces(Some(&first_project))
        .await
        .expect("first");
    assert_eq!(only_first, vec![ws]);
    let only_second = env
        .manager
        .list_workspaces(Some(&second.id))
        .await
        .expect("second");
    assert_eq!(only_second, vec![local]);
}
