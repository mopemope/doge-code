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

pub fn fs_search(
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
