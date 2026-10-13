use super::*;
use crate::{config::AppConfig, tools::FsTools};
use std::sync::Arc;
use tokio::sync::RwLock;

async fn invalid_late_step_preserves_source(first_shell: bool, mixed: bool) -> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join(".doge/workflows");
    std::fs::create_dir_all(&dir)?;
    let source = root.path().join("source.txt");
    std::fs::write(&source, "original\n")?;
    let first = if first_shell {
        "run: printf 'changed\\n' > source.txt"
    } else {
        "program: bash\n  args: ['-c', \"printf 'changed\\n' > source.txt\"]"
    };
    let bad = if mixed {
        "  run: echo invalid\n  program: echo\n"
    } else {
        ""
    };
    std::fs::write(
        dir.join("invalid.yml"),
        format!(
            "name: Invalid later step\nsteps:\n- name: Earlier edit\n  {first}\n- name: Invalid\n{bad}- name: Following\n  program: touch\n  args: [following]\n"
        ),
    )?;
    let cfg = AppConfig {
        project_root: root.path().into(),
        ..Default::default()
    };
    let tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
    let error = run_workflow("invalid", root.path(), &tools)
        .await
        .expect_err("invalid workflow");
    assert!(
        error.to_string().contains(if mixed {
            "both `run` and `program`"
        } else {
            "neither `run` nor `program`"
        }),
        "{error}"
    );
    assert_eq!(
        std::fs::read_to_string(source)?,
        "original\n",
        "invalid later shape must be detected before the first command runs"
    );
    assert!(!root.path().join("following").exists());
    assert!(
        error
            .to_string()
            .contains("at step 2 before any command ran")
    );
    Ok(())
}

#[tokio::test]
async fn mixed_after_shell_is_rejected_before_side_effects() -> Result<()> {
    invalid_late_step_preserves_source(true, true).await
}
#[tokio::test]
async fn mixed_after_process_is_rejected_before_side_effects() -> Result<()> {
    invalid_late_step_preserves_source(false, true).await
}
#[tokio::test]
async fn missing_after_shell_is_rejected_before_side_effects() -> Result<()> {
    invalid_late_step_preserves_source(true, false).await
}
#[tokio::test]
async fn missing_after_process_is_rejected_before_side_effects() -> Result<()> {
    invalid_late_step_preserves_source(false, false).await
}

#[tokio::test]
async fn valid_mixed_commands_keep_order_and_run_once() -> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join(".doge/workflows");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("valid.yml"),
        "name: Mixed valid\nsteps:\n- name: Shell\n  run: printf 'shell\\n' >> order\n- name: Process\n  program: bash\n  args: ['-c', \"printf 'process 日本語\\n' >> order\"]\n",
    )?;
    let cfg = AppConfig {
        project_root: root.path().into(),
        ..Default::default()
    };
    let tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
    assert!(
        run_workflow("valid", root.path(), &tools)
            .await?
            .contains("completed successfully")
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("order"))?,
        "shell\nprocess 日本語\n"
    );
    Ok(())
}

#[tokio::test]
async fn pre_cancelled_preflight_preserves_typed_cancellation_and_source() -> Result<()> {
    let root = tempfile::tempdir()?;
    let dir = root.path().join(".doge/workflows");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("cancel.yml"),
        "name: Cancel\nsteps:\n- name: Earlier\n  program: touch\n  args: [must-not-run]\n- name: Invalid\n",
    )?;
    let cfg = AppConfig {
        project_root: root.path().into(),
        ..Default::default()
    };
    let tools = FsTools::new(Arc::new(RwLock::new(None)), Arc::new(cfg));
    let token = CancellationToken::new();
    token.cancel();
    let error = run_workflow_with_cancel("cancel", root.path(), &tools, Some(token))
        .await
        .expect_err("cancel");
    assert!(error.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
    assert!(!root.path().join("must-not-run").exists());
    Ok(())
}

#[test]
fn invalid_shape_diagnostics_bound_the_name_without_echoing_commands() {
    let step = WorkflowStep {
        name: "\n\"\\日本語".repeat(10_000),
        run: Some("PRIVATE-SHELL-COMMAND".into()),
        program: Some("PRIVATE-PROGRAM".into()),
        args: vec!["PRIVATE-ARGUMENTS".into()],
        cwd: None,
        env: Default::default(),
        timeout_ms: None,
    };
    let error = match step.command() {
        Err(error) => error,
        Ok(_) => panic!("invalid shape"),
    };
    let serialized = json!({"error": error.to_string()}).to_string();
    assert!(serialized.chars().count() < 2000);
    assert!(!serialized.contains("PRIVATE-"));
}
