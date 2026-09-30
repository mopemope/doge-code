use crate::config::AppConfig;
use std::path::{Component, Path, PathBuf};

/// Portable canonical-path scope contract for file tools.
///
/// Authorization compares canonical-equivalent roots and targets:
/// every permitted root (`project_root`, each `allowed_paths` entry, and
/// any tool-specific extra root) is normalized with the same rules as the
/// requested target. This eliminates false denials when the configured root
/// is spelled through a symlink alias (e.g. macOS `/var` vs
/// `/private/var`) without weakening escape protection.
///
/// Contract:
/// - Absolute paths only; relative paths are denied.
/// - Existing targets resolve via `canonicalize` (symlinks resolved).
/// - Missing targets resolve via the nearest existing ancestor's
///   canonical form plus lexically-cleaned remainder, so a genuine in-root
///   new file is allowed while `..` escapes and symlink-to-outside parents
///   stay denied.
/// - Anything that cannot be proven in-scope is denied (fail closed).
/// - Scope-check failure never mutates files; callers must check before I/O.
pub fn ensure_in_scope(
    path: &Path,
    config: &AppConfig,
    extra_roots: &[PathBuf],
) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("File path must be absolute: {}", path.display());
    }
    let canonical_target = canonicalize_target(path).ok_or_else(|| {
        anyhow::anyhow!(
            "Access to files outside the project root is not allowed: {}",
            path.display()
        )
    })?;

    let mut roots: Vec<PathBuf> = Vec::with_capacity(2 + extra_roots.len());
    roots.push(canonicalize_root(&config.project_root));
    roots.extend(config.allowed_paths.iter().map(|p| canonicalize_root(p)));
    roots.extend(extra_roots.iter().map(|p| canonicalize_root(p)));

    if roots.iter().any(|root| canonical_target.starts_with(root)) {
        return Ok(canonical_target);
    }

    anyhow::bail!(
        "Access to files outside the project root is not allowed: {}",
        path.display()
    )
}

/// Scope check with no tool-specific extra roots (read/list/edit/apply-patch).
pub fn ensure_in_project_scope(path: &Path, config: &AppConfig) -> anyhow::Result<PathBuf> {
    ensure_in_scope(path, config, &[])
}

/// Normalize a permitted root with the same rules as targets:
/// canonicalize when it exists, otherwise lexically clean the spelling so
/// alias comparisons stay component-wise.
pub fn canonicalize_root(path: &Path) -> PathBuf {
    // Same contract as targets: resolve the nearest existing ancestor via
    // the OS so alias spellings agree, falling back to a lexical clean only
    // when nothing can be proven (fail-closed comparison downstream).
    if let Some(canonical) = canonicalize_target(path) {
        return canonical;
    }
    normalize_lexical(path)
}

/// Resolve a requested target to its canonical-equivalent form.
///
/// - Existing paths: full `canonicalize` (symlinks resolved by the OS).
/// - Missing paths: nearest existing ancestor is canonicalized and the
///   non-existent remainder is appended after lexical cleaning.
/// - Returns `None` when no existing ancestor can be canonicalized
///   (fail closed) or the remainder is unrepresentable.
pub fn canonicalize_target(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    // Walk up to the nearest existing ancestor, collecting the
    // non-existent remainder components. `ParentDir` (`..`) and `CurDir`
    // (`.`) components are preserved in the remainder (rather than dropped
    // via `file_name()`, which returns `None` for `..`-terminated paths)
    // so the final lexical clean resolves them against the canonical
    // ancestor. This is safe because the remainder does not exist and
    // therefore contains no symlinks; resolving `..` lexically here cannot
    // hide a symlink escape in the existing ancestor (already resolved by
    // the OS), while dropping `..` could mis-authorize the target.
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor: &Path = path;
    loop {
        if ancestor.exists() {
            // A remainder beneath a non-directory (e.g. `file.txt/../x`)
            // is unrepresentable (ENOTDIR): deny rather than lexically
            // collapsing through the file.
            if !remainder.is_empty() && !ancestor.is_dir() {
                return None;
            }
            let canonical_ancestor = ancestor.canonicalize().ok()?;
            let mut joined = canonical_ancestor;
            for component in remainder.iter().rev() {
                joined.push(component);
            }
            return Some(normalize_lexical(&joined));
        }
        let parent = ancestor.parent()?;
        if parent.as_os_str().is_empty() {
            return None;
        }
        match ancestor.components().next_back() {
            Some(Component::Normal(name)) => {
                remainder.push(name.to_os_string());
            }
            Some(Component::ParentDir) => {
                remainder.push(std::ffi::OsString::from(".."));
            }
            Some(Component::CurDir) => {
                // No-op: `.` does not change the resolved location.
            }
            Some(Component::RootDir) | Some(Component::Prefix(_)) | None => {
                return None;
            }
        }
        if parent == ancestor {
            return None;
        }
        ancestor = parent;
    }
}

/// Lexical normalization: resolve `.`/`..`/duplicate separators
/// component-wise without touching the filesystem. Used for roots that do
/// not exist and for the non-existent remainder of a target whose existing
/// ancestor was already canonicalized (so no symlink lives in the cleaned
/// portion).
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut is_absolute = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => {
                out.push(Component::RootDir.as_os_str());
                is_absolute = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let poppable = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if poppable {
                    out.pop();
                } else if !is_absolute {
                    out.push("..");
                }
                // Absolute `..` past the filesystem root collapses to root.
            }
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn config_with_root(root: &Path) -> AppConfig {
        AppConfig {
            project_root: root.to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn test_canonical_project_root_child_allowed() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let config = config_with_root(&root);
        let child = root.join("child.txt");
        fs::write(&child, "hi").unwrap();
        assert!(ensure_in_project_scope(&child, &config).is_ok());
    }

    #[test]
    fn test_lexical_root_canonical_target_alias_allowed() {
        // Simulate macOS `/var` vs `/private/var` aliasing with a symlink:
        // the configured root uses the lexical (symlinked) spelling while
        // the target canonicalizes through the link.
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let alias = dir.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        #[cfg(not(unix))]
        return;
        let config = config_with_root(&alias);
        let child = real.join("child.txt");
        fs::write(&child, "hi").unwrap();
        // Target spelled through the real path canonicalizes identically.
        assert!(ensure_in_project_scope(&child, &config).is_ok());
        // Target spelled through the alias also resolves identically.
        let alias_child = alias.join("child.txt");
        assert!(ensure_in_project_scope(&alias_child, &config).is_ok());
    }

    #[test]
    fn test_allowed_path_alias_normalized() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let real_allowed = dir.path().join("real_allowed");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&real_allowed).unwrap();
        #[cfg(not(unix))]
        return;
        #[cfg(unix)]
        {
            let alias = dir.path().join("allowed_alias");
            std::os::unix::fs::symlink(&real_allowed, &alias).unwrap();
            let mut config = config_with_root(&project);
            config.allowed_paths.push(alias);
            let child = real_allowed.join("file.txt");
            fs::write(&child, "hi").unwrap();
            assert!(ensure_in_project_scope(&child, &config).is_ok());
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_existing_symlink_escape_denied() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "TOP-SECRET").unwrap();
        symlink(&outside, project.join("link")).unwrap();
        let config = config_with_root(&project);
        let escape = project.join("link/secret.txt");
        assert!(ensure_in_project_scope(&escape, &config).is_err());
    }

    #[test]
    fn test_safe_new_child_allowed_and_escaping_new_target_denied() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let config = config_with_root(&project);
        // New file whose parent exists inside the root is allowed.
        let new_child = project.join("new_child.txt");
        assert!(!new_child.exists());
        assert!(ensure_in_project_scope(&new_child, &config).is_ok());
        // Sibling of the project root (not beneath it) is denied.
        let outside = dir.path().join("outside_new.txt");
        assert!(ensure_in_project_scope(&outside, &config).is_err());
        // New file with `..` escaping the root is denied.
        let escaping = project.join("../escaping.txt");
        assert!(ensure_in_project_scope(&escaping, &config).is_err());
        // Existing outside file is denied.
        let outside_file = dir.path().join("outside_existing.txt");
        fs::write(&outside_file, "secret").unwrap();
        assert!(ensure_in_project_scope(&outside_file, &config).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_new_child_under_symlink_to_outside_denied() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, project.join("link")).unwrap();
        let config = config_with_root(&project);
        // The parent `link` resolves outside, so a new file beneath it
        // must be denied even though the file itself does not exist yet.
        let new_escape = project.join("link/new_file.txt");
        assert!(!new_escape.exists());
        assert!(ensure_in_project_scope(&new_escape, &config).is_err());
    }

    #[test]
    fn test_relative_path_denied() {
        let dir = TempDir::new().unwrap();
        let config = config_with_root(dir.path());
        assert!(ensure_in_project_scope(Path::new("relative/file.txt"), &config).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_macos_temp_alias_regression() {
        // macOS spells temp dirs under `/var/...` while canonicalization
        // yields `/private/var/...`. Emulate with a symlinked root: the
        // config keeps the lexical spelling, the target is canonical.
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let canonical_tmp = dir.path().join("private_var_tmp");
        fs::create_dir_all(&canonical_tmp).unwrap();
        let lexical_tmp = dir.path().join("var_tmp");
        symlink(&canonical_tmp, &lexical_tmp).unwrap();
        let config = config_with_root(&lexical_tmp);
        let child = canonical_tmp.join("child.txt");
        fs::write(&child, "hi").unwrap();
        assert!(
            ensure_in_project_scope(&child, &config).is_ok(),
            "canonical-spelling target must not be rejected for a genuine in-root file"
        );
        // New (not-yet-created) child under the canonical spelling too.
        let new_child = canonical_tmp.join("new.txt");
        assert!(ensure_in_project_scope(&new_child, &config).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_unix_symlink_root_regression() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real_root");
        fs::create_dir_all(real.join("sub")).unwrap();
        fs::write(real.join("sub/file.txt"), "data").unwrap();
        let linked = dir.path().join("linked_root");
        symlink(&real, &linked).unwrap();
        // Config points at the symlink; target uses the real path.
        let config = config_with_root(&linked);
        assert!(ensure_in_project_scope(&real.join("sub/file.txt"), &config).is_ok());
        // And vice versa.
        let config2 = config_with_root(&real);
        assert!(ensure_in_project_scope(&linked.join("sub/file.txt"), &config2).is_ok());
    }

    #[test]
    fn test_extra_roots_are_scoped_per_call() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let extra = dir.path().join("extra");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&extra).unwrap();
        let child = extra.join("file.txt");
        fs::write(&child, "hi").unwrap();
        let config = config_with_root(&project);
        // Without the extra root it is denied; with it, allowed.
        assert!(ensure_in_project_scope(&child, &config).is_err());
        assert!(ensure_in_scope(&child, &config, &[extra]).is_ok());
    }

    #[test]
    fn test_dotdot_remainder_staying_inside_allowed() {
        // `..` in a not-yet-created target must be preserved (not dropped):
        // a spelling that resolves back inside the root stays allowed.
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(project.join("sub")).unwrap();
        let project = project.canonicalize().unwrap();
        let config = config_with_root(&project);
        let inside = project.join("sub/../new_inside.txt");
        assert!(!inside.exists());
        assert!(
            ensure_in_project_scope(&inside, &config).is_ok(),
            "sub/../new_inside.txt resolves inside and must be allowed"
        );
        // Interior `.` is a no-op and stays allowed.
        let dotted = project.join("sub/./new_dotted.txt");
        assert!(ensure_in_project_scope(&dotted, &config).is_ok());
        // Non-existent intermediate with `..` that lexically resolves back
        // inside must not be dropped to `None` (false denial): the
        // remainder `..` is preserved and cleaned against the canonical
        // ancestor.
        let via_missing = project.join("missing_mid/../new_via_missing.txt");
        assert!(!via_missing.exists());
        assert!(
            ensure_in_project_scope(&via_missing, &config).is_ok(),
            "missing_mid/../new_via_missing.txt resolves inside and must be allowed"
        );
    }

    #[test]
    fn test_dotdot_remainder_escaping_denied() {
        // `..` that escapes the root must be denied even though the file
        // does not exist yet and the existing ancestor is in-scope.
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(project.join("sub")).unwrap();
        let project = project.canonicalize().unwrap();
        let config = config_with_root(&project);
        let escaping = project.join("sub/../../escaping.txt");
        assert!(!escaping.exists());
        assert!(ensure_in_project_scope(&escaping, &config).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_dotdot_through_symlink_denied() {
        // `link -> outside`: `link/../new.txt` resolves against the link
        // target's parent (outside the project), not lexically inside.
        use std::os::unix::fs::symlink;
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, project.join("link")).unwrap();
        let config = config_with_root(&project);
        let traversal = project.join("link/../traversal_new.txt");
        assert!(!traversal.exists());
        // Must be denied: the OS resolves `link/..` via the link target.
        assert!(ensure_in_project_scope(&traversal, &config).is_err());
    }

    #[test]
    fn test_traversal_through_file_denied() {
        // `file.txt/../sibling` is unrepresentable (ENOTDIR): the existing
        // ancestor is a file, so the target must be denied rather than
        // lexically collapsed to the sibling inside the root.
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        fs::create_dir_all(&project).unwrap();
        let file = project.join("real_file.txt");
        fs::write(&file, "hi").unwrap();
        let project = project.canonicalize().unwrap();
        let config = config_with_root(&project);
        let traversal = project.join("real_file.txt/../sibling_new.txt");
        assert!(ensure_in_project_scope(&traversal, &config).is_err());
    }

    /// Consistent read/list/edit/apply-patch/write contract under a
    /// symlinked (macOS-alias-like) project root spelling.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_consistent_tool_contract_with_aliased_root() {
        use std::os::unix::fs::symlink;

        use crate::tools::{apply_patch, edit, list, read, write};

        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real_project");
        fs::create_dir_all(&real).unwrap();
        let alias = dir.path().join("alias_project");
        symlink(&real, &alias).unwrap();
        // Config keeps the lexical (aliased) spelling, targets use both.
        let config = config_with_root(&alias);
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "TOP-SECRET").unwrap();
        symlink(&outside, real.join("link")).unwrap();

        let s = |p: &std::path::Path| p.to_string_lossy().to_string();

        // read: canonical-spelled child allowed, outside + escape denied.
        let read_file = real.join("read_me.txt");
        fs::write(&read_file, "line1\nline2\n").unwrap();
        assert!(
            read::fs_read(&s(&read_file), read::FsReadOptions::default(), &config).is_ok(),
            "read must allow canonical-spelled in-root file"
        );
        assert!(
            read::fs_read(
                &s(&outside.join("secret.txt")),
                read::FsReadOptions::default(),
                &config
            )
            .is_err()
        );
        assert!(
            read::fs_read(
                &s(&real.join("link/secret.txt")),
                read::FsReadOptions::default(),
                &config
            )
            .is_err(),
            "read must deny symlink escapes"
        );

        // list: aliased root itself allowed; escape dir denied.
        assert!(
            list::fs_list(
                &s(&alias),
                Some(1),
                None,
                &config,
                list::FsListOptions::default()
            )
            .is_ok()
        );
        assert!(
            list::fs_list(
                &s(&real.join("link")),
                Some(1),
                None,
                &config,
                list::FsListOptions::default()
            )
            .is_err(),
            "list must deny symlink escapes"
        );

        // edit: canonical-spelled child allowed; scope failure mutates nothing.
        let edit_file = real.join("edit_me.txt");
        fs::write(&edit_file, "hello world").unwrap();
        let params = edit::EditParams {
            file_path: s(&edit_file),
            target_block: "world".to_string(),
            new_block: "Rust".to_string(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        };
        let result = edit::edit(params, &config).await.unwrap();
        assert!(result.success);
        assert!(ensure_in_project_scope(&outside.join("secret.txt"), &config).is_err());

        // apply_patch: canonical-spelled child allowed.
        let patch_file = real.join("patch_me.txt");
        fs::write(&patch_file, "line A\nline B\n").unwrap();
        let patch = diffy::create_patch("line A\nline B\n", "line A\nline Bee\n").to_string();
        let result = apply_patch::apply_patch(
            apply_patch::ApplyPatchParams {
                file_path: s(&patch_file),
                patch_content: patch,
            },
            &config,
        )
        .await
        .unwrap();
        assert!(result.success);

        // write: new in-root file (canonical spelling) allowed.
        assert!(
            write::fs_write(&s(&real.join("new_file.txt")), "new", &config).is_ok(),
            "write must allow new canonical-spelled in-root file"
        );
        // `fs_write` intentionally allows the system temp dir as an extra
        // root, so outside-denial cases must live outside both the project
        // and the system temp dir. Use a never-created sibling of the
        // system temp dir's parent (no files are written; the check denies
        // first). This holds on every platform even if the crate directory
        // itself lives under the system temp dir.
        let outside_base = std::env::temp_dir()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let outside_root = outside_base.join(format!(
            "dgc_scope_contract_denied_{}_outside",
            std::process::id()
        ));
        let denied_target = outside_root.join("new.txt");
        assert!(write::fs_write(&s(&denied_target), "new", &config).is_err());
        assert!(
            !denied_target.exists(),
            "denied write must not create files"
        );
        // Symlink escape whose target is outside both roots denied.
        symlink(&outside_base, real.join("link2")).unwrap();
        assert!(
            write::fs_write(&s(&real.join("link2/new.txt")), "new", &config).is_err(),
            "write must deny new files under symlink escapes"
        );
    }
}
