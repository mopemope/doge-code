use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use dgc_daemon::config::{DEFAULT_EVENTS_MAX_BYTES, DaemonConfig};
use dgc_daemon::lock::{InstanceLock, LockError};
use dgc_daemon::paths::{DaemonPaths, PathsError, ensure_private_dir};
use dgc_daemon::store::{Store, write_atomic};
use dgc_protocol::ids::{AgentId, ProjectId, WorkspaceId};
use dgc_protocol::model::{
    Agent, AgentState, Isolation, Project, ProviderKind, Workspace, WorkspaceState,
};
use tempfile::tempdir;

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn sample_project() -> Project {
    Project {
        id: ProjectId::generate(),
        path: PathBuf::from("/work/repo"),
        name: "repo".into(),
        created_at: "2026-11-25T00:00:00Z".into(),
    }
}

fn sample_workspace(project_id: &ProjectId) -> Workspace {
    Workspace {
        id: WorkspaceId::generate(),
        project_id: project_id.clone(),
        isolation: Isolation::Worktree,
        path: PathBuf::from("/work/wt"),
        branch: Some("dgc/x".into()),
        base_ref: Some("main".into()),
        state: WorkspaceState::Active,
        owned: true,
        created_at: "2026-11-25T00:00:00Z".into(),
    }
}

fn sample_agent(workspace_id: &WorkspaceId) -> Agent {
    let mut labels = BTreeMap::new();
    labels.insert("k".to_owned(), "v".to_owned());
    Agent {
        id: AgentId::generate(),
        workspace_id: workspace_id.clone(),
        title: "fix bug".into(),
        provider: ProviderKind::Claude,
        model: Some("opus".into()),
        mode: None,
        auto_approve: false,
        provider_session_id: Some("sess-1".into()),
        state: AgentState::Idle,
        parent_id: None,
        labels,
        created_at: "2026-11-25T00:00:00Z".into(),
        last_error: None,
    }
}

fn open_store(root: &Path) -> Store {
    Store::open(&root.join("state")).expect("open store")
}

#[test]
fn store_roundtrip_agent() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let ws = WorkspaceId::generate();
    let agent = sample_agent(&ws);

    assert_eq!(store.get_agent(&agent.id).expect("get"), None);
    store.put_agent(&agent).expect("put");
    assert_eq!(
        store.get_agent(&agent.id).expect("get"),
        Some(agent.clone())
    );

    // Overwrite replaces.
    let mut changed = agent.clone();
    changed.state = AgentState::Running;
    store.put_agent(&changed).expect("overwrite");
    assert_eq!(store.get_agent(&agent.id).expect("get"), Some(changed));

    let raw = fs::read_to_string(
        dir.path()
            .join("state/agents")
            .join(format!("{}.json", agent.id.as_str())),
    )
    .expect("read file");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(value["version"], 1);
    assert!(value["data"].is_object());

    assert!(store.remove_agent(&agent.id).expect("remove"));
    assert!(!store.remove_agent(&agent.id).expect("remove again"));
    assert_eq!(store.get_agent(&agent.id).expect("get"), None);
}

#[test]
fn store_roundtrip_project_and_workspace_with_listing() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());

    let p1 = sample_project();
    let p2 = sample_project();
    store.put_project(&p2).expect("put p2");
    store.put_project(&p1).expect("put p1");
    let mut listed = store.list_projects().expect("list");
    listed.sort_by(|a, b| a.id.cmp(&b.id));
    let mut expected = vec![p1.clone(), p2.clone()];
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(listed, expected);

    let w = sample_workspace(&p1.id);
    store.put_workspace(&w).expect("put workspace");
    assert_eq!(store.list_workspaces().expect("list"), vec![w.clone()]);
    assert_eq!(store.get_workspace(&w.id).expect("get"), Some(w.clone()));
    assert!(store.remove_workspace(&w.id).expect("remove"));
    assert!(store.list_workspaces().expect("list").is_empty());
    assert!(store.list_agents().expect("list").is_empty());

    assert!(store.remove_project(&p1.id).expect("remove"));
    assert_eq!(store.list_projects().expect("list"), vec![p2]);
}

#[test]
fn store_files_are_0600() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let project = sample_project();
    store.put_project(&project).expect("put");

    let state = dir.path().join("state");
    let file = state
        .join("projects")
        .join(format!("{}.json", project.id.as_str()));
    assert_eq!(mode_of(&file), 0o600);
    assert_eq!(mode_of(&state), 0o700);
    for sub in ["projects", "workspaces", "agents"] {
        assert_eq!(mode_of(&state.join(sub)), 0o700, "{sub}");
    }
    // No temp files are left behind.
    let leftovers: Vec<_> = fs::read_dir(state.join("projects"))
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
}

#[test]
fn store_refuses_symlink_target() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let project = sample_project();

    let victim = dir.path().join("victim.txt");
    fs::write(&victim, "precious").expect("write victim");
    let target = dir
        .path()
        .join("state/projects")
        .join(format!("{}.json", project.id.as_str()));
    symlink(&victim, &target).expect("symlink");

    assert!(store.put_project(&project).is_err());
    assert_eq!(fs::read_to_string(&victim).expect("read"), "precious");
    assert!(
        fs::symlink_metadata(&target)
            .expect("meta")
            .file_type()
            .is_symlink(),
        "symlink must be left in place"
    );
    assert!(store.get_project(&project.id).is_err());
}

#[test]
fn store_refuses_symlinked_entity_dir() {
    let dir = tempdir().expect("tempdir");
    let state = dir.path().join("state");
    drop(Store::open(&state).expect("open"));
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("mkdir");
    fs::remove_dir(state.join("agents")).expect("rmdir");
    symlink(&elsewhere, state.join("agents")).expect("symlink");

    assert!(Store::open(&state).is_err());
    let err = write_atomic(&state.join("agents/x.json"), b"{}");
    assert!(err.is_err());
    assert_eq!(fs::read_dir(&elsewhere).expect("read_dir").count(), 0);
}

#[test]
fn store_rejects_unknown_version() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let project = sample_project();
    store.put_project(&project).expect("put");

    let file = dir
        .path()
        .join("state/projects")
        .join(format!("{}.json", project.id.as_str()));
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).expect("read")).expect("json");
    value["version"] = serde_json::json!(2);
    fs::write(&file, serde_json::to_vec(&value).expect("encode")).expect("rewrite");

    assert!(store.get_project(&project.id).is_err());
    assert!(store.list_projects().is_err());
}

#[test]
fn store_rejects_corrupt_file() {
    let dir = tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let project = sample_project();
    store.put_project(&project).expect("put");
    let file = dir
        .path()
        .join("state/projects")
        .join(format!("{}.json", project.id.as_str()));
    fs::write(&file, "not json").expect("corrupt");
    assert!(store.get_project(&project.id).is_err());
}

#[test]
fn write_atomic_replaces_existing_file_and_keeps_mode() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("f.json");
    write_atomic(&path, b"one").expect("first");
    write_atomic(&path, b"two").expect("second");
    assert_eq!(fs::read(&path).expect("read"), b"two");
    assert_eq!(mode_of(&path), 0o600);
    assert_eq!(fs::read_dir(dir.path()).expect("read_dir").count(), 1);
}

#[test]
fn lock_second_acquire_fails() {
    let dir = tempdir().expect("tempdir");
    let first = InstanceLock::acquire(dir.path()).expect("first acquire");
    let pid = fs::read_to_string(dir.path().join("daemon.lock")).expect("read pid");
    assert_eq!(pid.trim(), std::process::id().to_string());

    let second = InstanceLock::acquire(dir.path());
    assert!(
        matches!(second, Err(LockError::AlreadyRunning)),
        "{second:?}"
    );
    // A failed attempt must not clobber the holder's pid record.
    let pid = fs::read_to_string(dir.path().join("daemon.lock")).expect("read pid");
    assert_eq!(pid.trim(), std::process::id().to_string());

    drop(first);
    InstanceLock::acquire(dir.path()).expect("acquire after release");
}

#[test]
fn private_dir_creates_0700_and_tightens_existing() {
    let dir = tempdir().expect("tempdir");
    let nested = dir.path().join("a/b/c");
    ensure_private_dir(&nested).expect("create");
    assert_eq!(mode_of(&nested), 0o700);

    let loose = dir.path().join("loose");
    fs::create_dir(&loose).expect("mkdir");
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o755)).expect("chmod");
    ensure_private_dir(&loose).expect("tighten");
    assert_eq!(mode_of(&loose), 0o700);
}

#[test]
fn private_dir_rejects_symlink() {
    let dir = tempdir().expect("tempdir");
    let real = dir.path().join("real");
    fs::create_dir(&real).expect("mkdir");
    let link = dir.path().join("link");
    symlink(&real, &link).expect("symlink");
    assert!(ensure_private_dir(&link).is_err());
}

#[test]
fn private_dir_rejects_regular_file() {
    let dir = tempdir().expect("tempdir");
    let file = dir.path().join("file");
    fs::write(&file, "x").expect("write");
    assert!(ensure_private_dir(&file).is_err());
}

fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let map: BTreeMap<String, OsString> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), OsString::from(*v)))
        .collect();
    move |name| map.get(name).cloned()
}

#[test]
fn paths_prefer_xdg_variables() {
    let paths = DaemonPaths::from_vars(vars(&[
        ("HOME", "/home/u"),
        ("XDG_STATE_HOME", "/xs"),
        ("XDG_CONFIG_HOME", "/xc"),
        ("XDG_DATA_HOME", "/xd"),
        ("XDG_RUNTIME_DIR", "/run/user/1000"),
    ]))
    .expect("paths");
    assert_eq!(paths.state_dir, Path::new("/xs/dgc"));
    assert_eq!(paths.config_file, Path::new("/xc/dgc/config.toml"));
    assert_eq!(paths.worktrees_dir, Path::new("/xd/dgc/worktrees"));
    assert_eq!(paths.socket, Path::new("/run/user/1000/dgc/daemon.sock"));
}

#[test]
fn paths_fall_back_to_home() {
    let paths = DaemonPaths::from_vars(vars(&[("HOME", "/home/u")])).expect("paths");
    assert_eq!(paths.state_dir, Path::new("/home/u/.local/state/dgc"));
    assert_eq!(
        paths.config_file,
        Path::new("/home/u/.config/dgc/config.toml")
    );
    assert_eq!(
        paths.worktrees_dir,
        Path::new("/home/u/.local/share/dgc/worktrees")
    );
    assert_eq!(
        paths.socket,
        Path::new("/home/u/.local/state/dgc/run/daemon.sock")
    );
}

#[test]
fn paths_ignore_empty_and_relative_xdg_values() {
    let paths = DaemonPaths::from_vars(vars(&[
        ("HOME", "/home/u"),
        ("XDG_STATE_HOME", ""),
        ("XDG_CONFIG_HOME", "relative/dir"),
        ("XDG_RUNTIME_DIR", ""),
    ]))
    .expect("paths");
    assert_eq!(paths.state_dir, Path::new("/home/u/.local/state/dgc"));
    assert_eq!(
        paths.config_file,
        Path::new("/home/u/.config/dgc/config.toml")
    );
    assert_eq!(
        paths.socket,
        Path::new("/home/u/.local/state/dgc/run/daemon.sock")
    );
}

#[test]
fn paths_without_home_or_xdg_is_error() {
    let err = DaemonPaths::from_vars(vars(&[])).expect_err("no home");
    assert!(matches!(err, PathsError::HomeUnset), "{err:?}");
    let err = DaemonPaths::from_vars(vars(&[("HOME", "")])).expect_err("empty home");
    assert!(matches!(err, PathsError::HomeUnset), "{err:?}");
}

#[test]
fn socket_path_too_long_is_error() {
    let long = format!("/tmp/{}", "a".repeat(100));
    let err = DaemonPaths::from_vars(vars(&[("HOME", "/home/u"), ("XDG_RUNTIME_DIR", &long)]))
        .expect_err("too long");
    assert!(
        matches!(err, PathsError::SocketPathTooLong { .. }),
        "{err:?}"
    );

    // Exactly at the limit is accepted: "/r/dgc/daemon.sock" is 18 bytes.
    let base = format!("/{}", "r".repeat(100 - 18 + 1));
    let socket_len = base.len() + "/dgc/daemon.sock".len();
    assert_eq!(socket_len, 100);
    DaemonPaths::from_vars(vars(&[("HOME", "/home/u"), ("XDG_RUNTIME_DIR", &base)]))
        .expect("at the limit");
}

#[test]
fn paths_under_root_stay_under_root() {
    let root = Path::new("/some/root");
    let paths = DaemonPaths::under(root);
    for p in [
        &paths.socket,
        &paths.state_dir,
        &paths.config_file,
        &paths.worktrees_dir,
    ] {
        assert!(p.starts_with(root), "{p:?}");
    }
}

#[test]
fn config_missing_file_gives_defaults() {
    let dir = tempdir().expect("tempdir");
    let config = DaemonConfig::load(&dir.path().join("none.toml")).expect("load");
    assert_eq!(config, DaemonConfig::default());
    assert_eq!(config.events_max_bytes, DEFAULT_EVENTS_MAX_BYTES);
    assert_eq!(config.events_max_bytes, 32 * 1024 * 1024);
    assert_eq!(config.claude_path, None);
}

#[test]
fn config_reads_values() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "claude_path = \"/opt/claude\"\nevents_max_bytes = 2048\n",
    )
    .expect("write");
    let config = DaemonConfig::load(&path).expect("load");
    assert_eq!(config.claude_path, Some(PathBuf::from("/opt/claude")));
    assert_eq!(config.codex_path, None);
    assert_eq!(config.events_max_bytes, 2048);
}

#[test]
fn config_unknown_key_is_error() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, "claude_pth = \"/opt/claude\"\n").expect("write");
    assert!(DaemonConfig::load(&path).is_err());
}

#[test]
fn config_syntax_error_is_error() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, "events_max_bytes = [\n").expect("write");
    assert!(DaemonConfig::load(&path).is_err());
}
