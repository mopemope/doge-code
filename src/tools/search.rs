use anyhow::{Context, Result};
use regex::Regex;
use std::path::PathBuf;

pub fn fs_search(
    root: &PathBuf,
    search_pattern: &str,
    file_glob: Option<&str>,
) -> Result<Vec<(PathBuf, usize, String)>> {
    let re = Regex::new(search_pattern).context("invalid regex")?;
    let mut results = Vec::new();
    let walker = globwalk::GlobWalkerBuilder::from_patterns(root, &[file_glob.unwrap_or("**/*")])
        .follow_links(false)
        .case_insensitive(true)
        .build()
        .context("build glob walker")?;
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let p = entry.path().to_path_buf();
        if entry.file_type().is_dir() {
            continue;
        }
        if re.is_match(p.to_str().unwrap_or_default()) {
            results.push((p.clone(), 0, p.display().to_string()));
            continue;
        }
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            let bin_exts = [
                "png", "jpg", "jpeg", "gif", "webp", "bmp", "pdf", "zip", "gz", "tar", "xz", "zst",
            ];
            if bin_exts.contains(&ext) {
                continue;
            }
        }
        let content = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                results.push((p.clone(), i + 1, line.to_string()));
            }
        }
    }
    Ok(results)
}
