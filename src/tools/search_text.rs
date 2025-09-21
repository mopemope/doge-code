use crate::llm::types::{ToolDef, ToolFunctionDef};

use anyhow::{Context, Result};

use glob::glob;

use serde::Deserialize;

use serde_json::json;

use std::path::PathBuf;

use std::process::Command;

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_text".to_string(),
            description: "Searches for a regular expression `search_pattern` within the content of files matching the `file_glob` pattern. It returns matching lines along with their file paths and line numbers. This tool is specifically for searching within file contents, not file names. For example, use it to locate all usages of a specific API, trace the origin of an error message, or find where a particular variable name is used. The `file_glob` argument is mandatory and must include a file extension to scope the search precisely.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "search_pattern": {
                        "type": "string",
                        "description": "The regular expression to search for within file contents."
                    },
                    "file_glob": {
                        "type": "string",
                        "description": "A glob pattern to filter which files are searched. This pattern must include a file extension. Examples: 'src/**/*.rs', 'tests/*.test.ts', '*.toml'."
                    }
                },
                "required": ["search_pattern", "file_glob"]
            }),
        },
    }
}

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
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(glob_pattern) = file_glob {
        // Expand the glob pattern to get a list of files
        for entry in glob(glob_pattern).context("Failed to read glob pattern")? {
            match entry {
                Ok(path) => {
                    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
                    if canonical_path.starts_with(&project_root) {
                        // Ensure the path is absolute
                        let absolute_path = path.canonicalize().unwrap_or(path);
                        // Add each file path as an argument to ripgrep
                        cmd.arg(absolute_path);
                    }
                    // If the path is outside the project root, we simply don't add it to the command
                }
                Err(e) => println!("Error reading glob entry: {e}"),
            }
        }
    } else {
        // If no glob pattern is provided, search in the current directory
        cmd.arg(".");
    }
    // Spawn ripgrep and stream its stdout to avoid loading everything into memory
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ripgrep")?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture ripgrep stdout")?;

    let reader = BufReader::new(stdout);

    let mut results = Vec::new();
    let mut bytes_read: usize = 0;

    const MAX_OUTPUT_BYTES: usize = 1_048_576; // 1 MiB

    for line_res in reader.lines() {
        let line = line_res.context("failed to read ripgrep output")?;
        // Track bytes read from ripgrep and stop if exceeding the limit
        bytes_read = bytes_read.saturating_add(line.len());
        if bytes_read > MAX_OUTPUT_BYTES {
            // try to terminate the child process
            let _ = child.kill();
            break;
        }

        if let Ok(parsed) = serde_json::from_str::<RipgrepJson>(&line)
            && let RipgrepMessageType::Match = parsed.r#type
            && let (Some(path_text), Some(lines_text), Some(line_number)) =
                (parsed.data.path, parsed.data.lines, parsed.data.line_number)
        {
            results.push((
                PathBuf::from(path_text.text),
                line_number,
                lines_text.text.trim().to_string(),
            ));
        }
    }

    // Ensure child process has exited
    let _ = child.wait();

    Ok(results)
}

#[cfg(test)]
mod tests {
    use crate::tools::search_text::search_text;
    use std::fs;
    use std::path::PathBuf;

    fn create_temp_dir() -> PathBuf {
        let temp_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("temp");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("test_")
            .tempdir_in(&temp_dir)
            .unwrap();
        #[allow(deprecated)]
        dir.into_path()
    }

    #[test]
    fn test_search_text_simple() {
        let root = create_temp_dir();
        fs::write(root.join("test.txt"), "hello world\nsecond line").unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let results = search_text("hello", Some(&file_glob)).unwrap();
        assert_eq!(results.len(), 1);
        let (path, line, content) = &results[0];
        assert_eq!(path, &root.join("test.txt"));
        assert_eq!(*line, 1);
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_fs_search_with_glob() {
        let root = create_temp_dir();
        fs::write(root.join("a.txt"), "find me").unwrap();
        fs::write(root.join("b.log"), "find me").unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let results = search_text("find me", Some(&file_glob)).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, root.join("a.txt"));
    }

    #[test]
    fn test_fs_search_no_match() {
        let root = create_temp_dir();
        fs::write(root.join("test.txt"), "some content").unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let results = search_text("nonexistent", Some(&file_glob)).unwrap();
        assert!(results.is_empty());
    }
}
