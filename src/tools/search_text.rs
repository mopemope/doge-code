use anyhow::{Context, Result};
use glob::glob;
use serde::Deserialize;
use std::path::PathBuf;
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
    search_pattern: &str,
    file_glob: Option<&str>,
) -> Result<Vec<(PathBuf, usize, String)>> {
    let mut cmd = Command::new("rg");
    cmd.arg("--json").arg("-n").arg("-e").arg(search_pattern);

    if let Some(glob_pattern) = file_glob {
        // Expand the glob pattern to get a list of files
        for entry in glob(glob_pattern).context("Failed to read glob pattern")? {
            match entry {
                Ok(path) => {
                    // Add each file path as an argument to ripgrep
                    cmd.arg(path);
                }
                Err(e) => println!("Error reading glob entry: {e}"),
            }
        }
    } else {
        // If no glob pattern is provided, search in the current directory
        cmd.arg(".");
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

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/{}.txt", root_str, "*");
        let results = search_text("hello", Some(&file_glob)).unwrap();
        assert_eq!(results.len(), 1);
        let (path, line, content) = &results[0];
        assert_eq!(path, &root.join("test.txt"));
        assert_eq!(*line, 1);
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_fs_search_with_glob() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), "find me").unwrap();
        fs::write(root.join("b.log"), "find me").unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/{}.txt", root_str, "*");
        let results = search_text("find me", Some(&file_glob)).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, root.join("a.txt"));
    }

    #[test]
    fn test_fs_search_no_match() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("test.txt"), "some content").unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/{}.txt", root_str, "*");
        let results = search_text("nonexistent", Some(&file_glob)).unwrap();
        assert!(results.is_empty());
    }
}
