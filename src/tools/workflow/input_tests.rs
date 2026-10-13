use super::*;
use crate::{config::AppConfig, tools::FsTools};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::test]
async fn oversized_yaml_is_rejected_before_any_command_runs() -> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join(".doge/workflows");
    std::fs::create_dir_all(&dir)?;
    let text = format!(
        "name: Oversize\nsteps:\n- name: MustNotRun\n  program: touch\n  args: [must-not-run]\n#{}",
        "x".repeat(1024 * 1024)
    );
    std::fs::write(dir.join("oversize.yml"), &text)?;
    let config = Arc::new(AppConfig {
        project_root: root.path().into(),
        ..Default::default()
    });
    let tools = FsTools::new(Arc::new(RwLock::new(None)), config);
    assert!(
        run_workflow("oversize", root.path(), &tools).await.is_err(),
        "over-limit valid YAML must be rejected before execution"
    );
    assert!(!root.path().join("must-not-run").exists());
    assert_eq!(std::fs::read_to_string(dir.join("oversize.yml"))?, text);
    Ok(())
}

fn fixture() -> (tempfile::TempDir, FsTools, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("root");
    let dir = root.path().join(".doge/workflows");
    std::fs::create_dir_all(&dir).expect("dir");
    let config = Arc::new(AppConfig {
        project_root: root.path().into(),
        ..Default::default()
    });
    let tools = FsTools::new(Arc::new(RwLock::new(None)), config);
    (root, tools, dir)
}
const VALID: &str =
    "name: Selected\nsteps:\n- name: Echo\n  program: echo\n  args: ['日本語 literal']\n";

#[tokio::test]
async fn exact_limit_is_readable_and_excess_is_rejected_without_source_changes() -> Result<()> {
    let (root, tools, dir) = fixture();
    let prefix = format!("{VALID}#");
    let exact = format!(
        "{prefix}{}",
        "x".repeat(MAX_WORKFLOW_INPUT_BYTES - prefix.len())
    );
    std::fs::write(dir.join("exact.yml"), &exact)?;
    assert!(
        run_workflow("exact", root.path(), &tools)
            .await?
            .contains("日本語 literal")
    );
    assert_eq!(std::fs::read_to_string(dir.join("exact.yml"))?, exact);
    std::fs::write(dir.join("exact.yml"), format!("{exact}x"))?;
    assert!(run_workflow("exact", root.path(), &tools).await.is_err());
    assert_eq!(
        std::fs::metadata(dir.join("exact.yml"))?.len(),
        MAX_WORKFLOW_INPUT_BYTES as u64 + 1
    );
    Ok(())
}

#[tokio::test]
async fn fallback_selects_only_missing_primary_and_preserves_present_invalid_primary() -> Result<()>
{
    let (root, tools, dir) = fixture();
    std::fs::write(dir.join("selected.yaml"), VALID)?;
    assert!(
        run_workflow("selected", root.path(), &tools)
            .await?
            .contains("日本語 literal")
    );
    std::fs::write(dir.join("selected.yml"), [0xff, 0xfe])?;
    let error = run_workflow("selected", root.path(), &tools)
        .await
        .expect_err("invalid primary");
    assert!(error.to_string().contains("UTF-8"));
    assert_eq!(std::fs::read(dir.join("selected.yml"))?, [0xff, 0xfe]);
    std::fs::write(dir.join("selected.yml"), "name: Primary\nsteps: []\n")?;
    assert!(
        run_workflow("selected", root.path(), &tools)
            .await?
            .contains("Running workflow: Primary")
    );
    Ok(())
}

#[tokio::test]
async fn invalid_inputs_are_bounded_and_do_not_echo_yaml_payload_or_create_directories()
-> Result<()> {
    let (root, tools, dir) = fixture();
    let error = run_workflow(&"long".repeat(10_000), root.path(), &tools)
        .await
        .expect_err("long path");
    assert!(
        json!({"error": error.to_string()})
            .to_string()
            .chars()
            .count()
            < 2000
    );
    std::fs::write(dir.join("private.yml"), "PRIVATE-YAML-PAYLOAD: [invalid")?;
    let error = run_workflow("private", root.path(), &tools)
        .await
        .expect_err("bad YAML");
    assert!(!error.to_string().contains("PRIVATE-YAML-PAYLOAD"));
    assert_eq!(
        std::fs::read_to_string(dir.join("private.yml"))?,
        "PRIVATE-YAML-PAYLOAD: [invalid"
    );
    std::fs::remove_dir_all(&dir)?;
    assert!(run_workflow("missing", root.path(), &tools).await.is_err());
    assert!(!dir.exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn regular_symlinks_work_and_fifo_primary_is_rejected_without_yaml_fallback() -> Result<()> {
    let (root, tools, dir) = fixture();
    std::fs::write(dir.join("regular.yml"), VALID)?;
    std::os::unix::fs::symlink(dir.join("regular.yml"), dir.join("alias.yml"))?;
    assert!(
        run_workflow("alias", root.path(), &tools)
            .await?
            .contains("日本語 literal")
    );
    let fifo = dir.join("fifo.yml");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())?;
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    std::fs::write(dir.join("fifo.yaml"), VALID)?;
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_workflow("fifo", root.path(), &tools),
    )
    .await
    .expect("must not block")
    .expect_err("FIFO primary");
    assert!(error.to_string().contains("not a regular file"));
    use std::os::unix::fs::FileTypeExt;
    assert!(std::fs::symlink_metadata(fifo)?.file_type().is_fifo());
    std::fs::create_dir(dir.join("directory.yml"))?;
    assert!(
        run_workflow("directory", root.path(), &tools)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn pre_cancelled_input_is_typed_and_never_acquired() -> Result<()> {
    let (root, tools, dir) = fixture();
    // A directory would normally cause a non-regular error; cancellation wins first.
    std::fs::create_dir(dir.join("cancel.yml"))?;
    let token = CancellationToken::new();
    token.cancel();
    let error = run_workflow_with_cancel("cancel", root.path(), &tools, Some(token))
        .await
        .expect_err("cancelled");
    assert!(matches!(
        error.downcast_ref::<crate::llm::LlmErrorKind>(),
        Some(crate::llm::LlmErrorKind::Cancelled)
    ));
    assert!(dir.join("cancel.yml").is_dir());
    Ok(())
}
