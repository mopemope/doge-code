//! Glob traversal with a cancellation boundary at each directory entry,
//! including entries that never yield a match. Match syntax remains glob's.
use anyhow::Result;
use glob::Pattern;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

pub(super) fn expand(pattern: &str, cancel: &CancellationToken) -> Result<Vec<PathBuf>> {
    expand_observed(pattern, cancel, |_| {})
}

fn expand_observed(
    pattern: &str,
    cancel: &CancellationToken,
    mut visited: impl FnMut(&std::path::Path),
) -> Result<Vec<PathBuf>> {
    super::async_io::check(Some(cancel))?;
    // Validate the complete expression as glob does, even if it has no matches.
    let _ = glob::glob(pattern)?;
    // Keep glob's raw component spellings (including dot components) and
    // platform prefix handling; Path::components alone normalizes too much.
    let mut components = std::path::Path::new(pattern).components().peekable();
    while matches!(
        components.peek(),
        Some(std::path::Component::Prefix(_) | std::path::Component::RootDir)
    ) {
        components.next();
    }
    let rest = components.map(|part| part.as_os_str()).collect::<PathBuf>();
    let normalized = std::path::Path::new(pattern).iter().collect::<PathBuf>();
    let root_len = normalized.to_string_lossy().len() - rest.to_string_lossy().len();
    let base = if root_len == 0 {
        PathBuf::from(".")
    } else {
        PathBuf::from(&pattern[..root_len])
    };
    #[cfg(windows)]
    if let Some(std::path::Component::Prefix(prefix)) = base.components().next()
        && prefix.kind().is_verbatim()
        && !matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_))
    {
        return Ok(Vec::new());
    }
    let require_dir = pattern
        .chars()
        .next_back()
        .is_some_and(std::path::is_separator);
    let mut parts = pattern[root_len.min(pattern.len())..]
        .split_terminator(std::path::is_separator)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if root_len == pattern.len() {
        parts.push(String::new());
    }
    let patterns = parts
        .iter()
        .map(|part| Pattern::new(part))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    enum Work {
        Expand(PathBuf, usize),
        Match(PathBuf, usize, bool),
        Emit(PathBuf),
    }
    let mut todo = vec![Work::Expand(base, 0)];
    let mut results = Vec::new();
    while let Some(work) = todo.pop() {
        super::async_io::check(Some(cancel))?;
        match work {
            Work::Expand(parent, index) => {
                if index >= parts.len() {
                    continue;
                }
                // Literal components retain glob's no-directory-scan behavior.
                if !parts[index].contains(['*', '?', '[']) {
                    let path = if parent == std::path::Path::new(".") {
                        PathBuf::from(&parts[index])
                    } else {
                        parent.join(&parts[index])
                    };
                    visited(&path);
                    super::async_io::check(Some(cancel))?;
                    if let Ok(metadata) =
                        std::fs::metadata(&path).or_else(|_| std::fs::symlink_metadata(&path))
                    {
                        if index + 1 == parts.len() {
                            if !require_dir || metadata.is_dir() {
                                results.push(path);
                            }
                        } else if metadata.is_dir() {
                            todo.push(Work::Expand(path, index + 1));
                        }
                    }
                    continue;
                }
                let directory = if parent.as_os_str().is_empty() {
                    std::path::Path::new(".")
                } else {
                    &parent
                };
                if !directory.is_dir() {
                    continue;
                }
                let mut children = Vec::new();
                for entry in std::fs::read_dir(directory)? {
                    super::async_io::check(Some(cancel))?;
                    let entry = entry?;
                    let path = if parent == std::path::Path::new(".") {
                        PathBuf::from(entry.file_name())
                    } else {
                        parent.join(entry.file_name())
                    };
                    visited(&path);
                    super::async_io::check(Some(cancel))?;
                    // glob ignores missing metadata (including broken symlinks).
                    let is_dir = std::fs::metadata(&path)
                        .map(|meta| meta.is_dir())
                        .unwrap_or(false);
                    children.push((path, is_dir));
                }
                children.sort_by(|a, b| a.0.cmp(&b.0));
                for (path, is_dir) in children.into_iter().rev() {
                    todo.push(Work::Match(path, index, is_dir));
                }
                // glob explicitly includes dot-directory entries only for a
                // literal leading dot, and pushes .. after . (LIFO order).
                if parts[index].starts_with('.') {
                    for special in [".", ".."] {
                        if patterns[index].matches(special) {
                            let path = parent.join(special);
                            if index + 1 == parts.len() {
                                todo.push(Work::Emit(path));
                            } else {
                                todo.push(Work::Expand(path, index + 1));
                            }
                        }
                    }
                }
            }
            Work::Emit(path) => results.push(path),
            Work::Match(path, mut index, is_dir) => {
                if parts[index] == "**" {
                    while index + 1 < parts.len() && parts[index + 1] == "**" {
                        index += 1;
                    }
                    if is_dir {
                        if index + 1 == parts.len() {
                            results.push(path.clone());
                            todo.push(Work::Expand(path, index));
                        } else {
                            todo.push(Work::Expand(path.clone(), index));
                            todo.push(Work::Match(path, index + 1, is_dir));
                        }
                        continue;
                    }
                    if index + 1 == parts.len() {
                        continue;
                    }
                    index += 1;
                }
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| patterns[index].matches(name))
                {
                    if index + 1 == parts.len() {
                        if !require_dir || is_dir {
                            results.push(path);
                        }
                    } else if is_dir {
                        todo.push(Work::Expand(path, index + 1));
                    }
                }
            }
        }
    }
    super::async_io::check(Some(cancel))?;
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellable_glob_matches_legacy_order_and_syntax() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "a.txt",
            ".hidden",
            "one/b.txt",
            "one/two/c.rs",
            "other/d.txt",
        ] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("one"), dir.path().join("alias")).unwrap();
        for suffix in [
            "",
            ".",
            "./",
            "*",
            "**",
            "**/*",
            "**/*.txt",
            "**/**/?.txt",
            "one/**/",
            "one/../*.txt",
            "[ao]*/*.txt",
            "one/two/c.rs",
            "one/./*.txt",
            "missing/*.txt",
            ".*",
            "**/*.no-match",
        ] {
            let pattern = format!("{}/{suffix}", dir.path().display());
            let expected = glob::glob(&pattern)
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                expand(&pattern, &CancellationToken::new()).unwrap(),
                expected,
                "{suffix}"
            );
        }
    }
    #[test]
    fn cancellable_glob_no_match_traversal_stops_before_full_tree() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..30 {
            std::fs::create_dir_all(dir.path().join(format!("{n}/child/deep"))).unwrap();
        }
        let token = CancellationToken::new();
        let mut count = 0;
        let error = expand_observed(
            &format!("{}/**/*.no-match", dir.path().display()),
            &token,
            |_| {
                count += 1;
                if count == 5 {
                    token.cancel();
                }
            },
        )
        .unwrap_err();
        assert_eq!(count, 5);
        assert!(matches!(
            error.downcast_ref::<crate::llm::LlmErrorKind>(),
            Some(crate::llm::LlmErrorKind::Cancelled)
        ));
    }
}
