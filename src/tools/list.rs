use anyhow::Result;
use globwalk::GlobWalkerBuilder;
use std::path::Path;

pub fn fs_list(root: &Path, path: &str, max_depth: Option<usize>, pattern: Option<&str>) -> Result<Vec<String>> {
    let full_path = root.join(path);
    let walker = GlobWalkerBuilder::new(full_path, pattern.unwrap_or("**/*"))
        .max_depth(max_depth.unwrap_or(usize::MAX))
        .build()?;

    let files = walker
        .filter_map(Result::ok)
        .map(|entry| entry.path().strip_prefix(root).unwrap_or(entry.path()).to_string_lossy().to_string())
        .collect();

    Ok(files)
}
