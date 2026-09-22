use percent_encoding::percent_decode_str;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Generic, non-leaking error for MCP resource path resolution.
///
/// Client-facing messages are intentionally generic (`Display`); detailed
/// reasons are emitted via `tracing` logs at the rejection site so host
/// filesystem layout is never echoed back to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResourcePathError {
    #[error("invalid project resource path")]
    InvalidPath,
    #[error("resource not found")]
    NotFound,
}

/// Resolve a `doge://files/...` / `doge://symbols/...` resource path strictly
/// inside `project_root`.
///
/// Contract: project-root-only. `allowed_paths` are intentionally *not*
/// honored here; the resource template promises
/// "Read the content of a file in the project."
///
/// Steps: strict percent-decode -> component validation -> join ->
/// canonicalize -> `starts_with` check -> file check.
///
/// Note: as with most path-check-then-use flows, a local writer with
/// filesystem access could theoretically swap the target between the
/// `canonicalize` check and the subsequent read. This is accepted risk:
/// such a writer already has equivalent local access, and the listener is
/// loopback-only with Host/Origin validation.
pub fn resolve_project_resource_path(
    project_root: &Path,
    encoded_path: &str,
) -> Result<PathBuf, ResourcePathError> {
    if encoded_path.is_empty() {
        tracing::warn!("resource path rejected: empty path");
        return Err(ResourcePathError::InvalidPath);
    }

    let decoded = percent_decode_str(encoded_path)
        .decode_utf8()
        .map_err(|e| {
            tracing::warn!("resource path rejected: invalid percent encoding/UTF-8: {e}");
            ResourcePathError::InvalidPath
        })?;
    let decoded = decoded.as_ref();

    if decoded.is_empty() {
        tracing::warn!("resource path rejected: empty decoded path");
        return Err(ResourcePathError::InvalidPath);
    }

    if decoded.contains('\0') {
        tracing::warn!("resource path rejected: NUL byte");
        return Err(ResourcePathError::InvalidPath);
    }

    // URI resource paths use `/`; backslash is rejected outright to block
    // Windows-style `..\` traversal on any host.
    if decoded.contains('\\') {
        tracing::warn!("resource path rejected: backslash in path");
        return Err(ResourcePathError::InvalidPath);
    }

    // Explicit Windows drive-letter check so `C:/...` / `C:...` is rejected
    // even on Unix hosts where `Component::Prefix` never fires.
    {
        let bytes = decoded.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            tracing::warn!("resource path rejected: windows drive prefix");
            return Err(ResourcePathError::InvalidPath);
        }
    }

    let rel = Path::new(decoded);

    // Reject absolute paths, `..`, `/`, and Windows prefixes after decode.
    // This runs on the *decoded* value so `%2e%2e` / `%2f` bypasses fail.
    if rel.is_absolute() {
        tracing::warn!("resource path rejected: absolute path");
        return Err(ResourcePathError::InvalidPath);
    }
    for component in rel.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                tracing::warn!("resource path rejected: forbidden component");
                return Err(ResourcePathError::InvalidPath);
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    let joined = project_root.join(rel);

    let canonical_root = project_root.canonicalize().map_err(|e| {
        tracing::warn!("resource path rejected: cannot canonicalize project root: {e}");
        ResourcePathError::InvalidPath
    })?;

    let canonical_target = joined.canonicalize().map_err(|_| {
        // Missing file: generic not-found, no host path leaked.
        tracing::debug!("resource path not found (missing target)");
        ResourcePathError::NotFound
    })?;

    if !canonical_target.starts_with(&canonical_root) {
        tracing::warn!("resource path rejected: escapes project root (symlink or traversal)");
        return Err(ResourcePathError::InvalidPath);
    }

    if !canonical_target.is_file() {
        tracing::debug!("resource path not found (not a file)");
        return Err(ResourcePathError::NotFound);
    }

    Ok(canonical_target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> (TempDir, PathBuf, PathBuf) {
        let root_tmp = TempDir::new().expect("tempdir");
        let project = root_tmp.path().join("project");
        let outside = root_tmp.path().join("outside");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(outside.join("secret.txt"), "TOP-SECRET\n").unwrap();
        let project = project.canonicalize().unwrap_or(project);
        (root_tmp, project, outside)
    }

    #[test]
    fn test_resource_valid_nested_file() {
        let (_tmp, project, _) = setup();
        let resolved = resolve_project_resource_path(&project, "src/main.rs").expect("valid");
        assert!(resolved.is_file());
        assert!(resolved.starts_with(project.canonicalize().unwrap()));
        let content = fs::read_to_string(&resolved).unwrap();
        assert!(content.contains("fn main"));
    }

    #[test]
    fn test_resource_parent_traversal_rejected() {
        let (_tmp, project, _) = setup();
        let err = resolve_project_resource_path(&project, "../outside/secret.txt").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
    }

    #[test]
    fn test_resource_encoded_parent_traversal_rejected() {
        let (_tmp, project, _) = setup();
        for encoded in [
            "%2e%2e/outside/secret.txt",
            "%2E%2E/outside/secret.txt",
            "..%2foutside%2fsecret.txt",
            "%2e%2e%2foutside%2fsecret.txt",
        ] {
            let err = resolve_project_resource_path(&project, encoded).unwrap_err();
            assert_eq!(
                err,
                ResourcePathError::InvalidPath,
                "encoding should be rejected: {encoded}"
            );
        }
    }

    #[test]
    fn test_resource_encoded_separator_rejected() {
        let (_tmp, project, _) = setup();
        let err =
            resolve_project_resource_path(&project, "%2e%2e%2foutside/secret.txt").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
        let err = resolve_project_resource_path(&project, "..%5coutside%5csecret.txt").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
    }

    #[test]
    fn test_resource_absolute_path_rejected() {
        let (_tmp, project, outside) = setup();
        let abs = outside.join("secret.txt");
        let abs_str = abs.to_string_lossy().to_string();
        let err = resolve_project_resource_path(&project, &abs_str).unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
        // Double-slash absolute form (`doge://files//etc/passwd` remainder).
        let err = resolve_project_resource_path(&project, "/etc/passwd").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
        let err = resolve_project_resource_path(&project, "//etc/passwd").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
    }

    #[test]
    fn test_resource_invalid_utf8_rejected() {
        let (_tmp, project, _) = setup();
        // %FF decodes to invalid UTF-8; must be rejected, never lossy-decoded.
        let err = resolve_project_resource_path(&project, "%FF").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
    }

    #[test]
    fn test_resource_empty_rejected() {
        let (_tmp, project, _) = setup();
        let err = resolve_project_resource_path(&project, "").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
    }

    #[test]
    #[cfg(unix)]
    fn test_resource_symlink_escape_rejected() {
        use std::os::unix::fs::symlink;
        let (_tmp, project, outside) = setup();
        let link = project.join("link");
        symlink(&outside, &link).unwrap();
        let err = resolve_project_resource_path(&project, "link/secret.txt").unwrap_err();
        assert_eq!(err, ResourcePathError::InvalidPath);
    }

    #[test]
    fn test_resource_error_does_not_leak_host_path() {
        let (_tmp, project, outside) = setup();
        let abs = outside.join("secret.txt");
        let abs_str = abs.to_string_lossy().to_string();
        let err = resolve_project_resource_path(&project, &abs_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains(&abs_str));
        assert!(!msg.contains("secret.txt"));
        assert!(!msg.contains("/tmp"));
        assert!(!msg.contains("/private"));
        assert!(!msg.contains("/Users"));
        assert!(!msg.contains("/home"));
    }

    #[test]
    fn test_resource_windows_drive_rejected() {
        let (_tmp, project, _) = setup();
        for candidate in [
            "C:/Windows/System32",
            "C:\\Windows\\System32",
            "D:/secret.txt",
        ] {
            let err = resolve_project_resource_path(&project, candidate).unwrap_err();
            assert_eq!(err, ResourcePathError::InvalidPath, "{candidate}");
        }
    }

    #[test]
    fn test_resource_directory_rejected() {
        let (_tmp, project, _) = setup();
        let err = resolve_project_resource_path(&project, "src").unwrap_err();
        assert_eq!(err, ResourcePathError::NotFound);
    }
}
