use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
enum RipgrepMessageType {
    Begin,
    End,
    Match,
    Context,
}

#[derive(Deserialize, Debug)]
struct RipgrepJson {
    r#type: RipgrepMessageType,
    data: RipgrepData,
}

#[derive(Deserialize, Debug)]
struct RipgrepData {
    path: Option<RipgrepText>,
    lines: Option<RipgrepText>,
    line_number: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct RipgrepText {
    text: String,
}

pub fn search_text(
    root: &Path,
    search_pattern: &str,
    file_glob: Option<&str>,
) -> Result<Vec<(PathBuf, usize, String)>> {
    let mut cmd = Command::new("rg");
    cmd.current_dir(root)
        .arg("--json")
        .arg("-n")
        .arg("-e")
        .arg(search_pattern);

    if let Some(glob) = file_glob {
        cmd.arg("-g").arg(glob);
    }

    let output = cmd.output().context("failed to execute ripgrep")?;
    let stdout = String::from_utf8(output.stdout).context("ripgrep output is not utf-8")?;

    let mut results = Vec::new();
    for line in stdout.lines() {
        if let Ok(json) = serde_json::from_str::<RipgrepJson>(line) {
            if let RipgrepMessageType::Match = json.r#type {
                if let (Some(path_text), Some(lines_text), Some(line_number)) =
                    (json.data.path, json.data.lines, json.data.line_number)
                {
                    results.push((
                        PathBuf::from(path_text.text),
                        line_number,
                        lines_text.text.trim().to_string(),
                    ));
                }
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use crate::tools::search_text::search_text;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_search_text_simple() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("test.txt"), "hello world\nsecond line").unwrap();

        let results = search_text(root, "hello", None).unwrap();
        assert_eq!(results.len(), 1);
        let (path, line, content) = &results[0];
        assert_eq!(path.to_str().unwrap(), "test.txt");
        assert_eq!(*line, 1);
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_fs_search_with_glob() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), "find me").unwrap();
        fs::write(root.join("b.log"), "find me").unwrap();

        let results = search_text(root, "find me", Some("*.txt")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.to_str().unwrap(), "a.txt");
    }

    #[test]
    fn test_fs_search_no_match() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("test.txt"), "some content").unwrap();

        let results = search_text(root, "nonexistent", None).unwrap();
        assert!(results.is_empty());
    }
}
