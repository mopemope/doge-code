use crate::config::AppConfig;
use std::path::PathBuf;

/// Creates a test AppConfig with the temp directory added to allowed_paths
/// This is needed for tests that create temporary files in the temp/ directory
/// which would otherwise be blocked by the path validation.
pub fn create_test_config_with_temp_dir() -> AppConfig {
    let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
    let mut config = AppConfig::default();
    config.allowed_paths.push(temp_dir);
    config
}

/// Wrapper function for fs_read that automatically creates a test config with temp dir allowed
pub fn test_fs_read(
    path: &str,
    start_line: Option<usize>,
    limit: Option<usize>,
) -> anyhow::Result<super::read::FsReadResult> {
    let config = create_test_config_with_temp_dir();
    let opts = super::read::FsReadOptions {
        start_line,
        limit,
        ..Default::default()
    };
    super::read::fs_read(path, opts, &config)
}
