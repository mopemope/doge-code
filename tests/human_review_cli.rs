use std::{
    fs,
    process::{Command, Stdio},
};
#[test]
fn human_review_noninteractive_does_not_load_auth_or_record_a_judgment() {
    let root = tempfile::tempdir().unwrap();
    let store = doge_fixture(root.path());
    let output = Command::new(env!("CARGO_BIN_EXE_dgc"))
        .current_dir(root.path())
        .env_clear()
        .env("DOGE_CODE_CONFIG", "/nonexistent/private-config")
        .args([
            "session",
            "review",
            &store,
            "accept",
            "--snapshot",
            &format!("review-v1:{}", "0".repeat(64)),
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interactive operator confirmation"),
        "{stderr}"
    );
    assert!(
        !root
            .path()
            .join(format!(".doge/sessions/{store}/human-review"))
            .exists()
    );
    assert!(!root.path().join(".doge/logs").exists());
}
fn doge_fixture(root: &std::path::Path) -> String {
    // No real session/config access: this path is deliberately absent and must
    // not be read before rejecting the noninteractive operator action.
    let id = "12345678-1234-4123-8123-123456789abc".to_string();
    fs::create_dir_all(root.join(".doge/sessions")).unwrap();
    id
}
