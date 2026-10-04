use crate::config::AppConfig;
use crate::llm::types::{ToolDef, ToolFunctionDef};

use anyhow::{Context, Result};

use serde::Deserialize;

use serde_json::json;

use std::path::Path;
use std::path::PathBuf;

use std::process::Command;

const MAX_OUTPUT_BYTES: usize = 1_048_576; // 1 MiB
const DEFAULT_MAX_RESULTS: usize = 200;
const HARD_MAX_RESULTS: usize = 2_000;
/// Per-match text cap so a single minified line cannot dominate the budget.
const MAX_MATCH_TEXT_CHARS: usize = 500;
/// Default response budget in chars; serialized JSON must stay under the
/// 8,000-char global truncation cap.
pub const DEFAULT_SEARCH_BUDGET_CHARS: usize = 6_000;
/// Rough per-result JSON overhead (path, line, punctuation, escaping).
const PER_RESULT_OVERHEAD: usize = 40;

#[derive(Debug, Clone, Copy)]
pub struct SearchTextOptions {
    pub max_results: Option<usize>,
    pub offset: Option<usize>,
    pub response_budget_chars: Option<usize>,
}

impl Default for SearchTextOptions {
    fn default() -> Self {
        Self {
            max_results: Some(DEFAULT_MAX_RESULTS),
            offset: Some(0),
            response_budget_chars: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchTextResult {
    pub rows: Vec<(PathBuf, usize, String)>,
    pub truncated: bool,
    pub next_offset: Option<usize>,
    pub offset: usize,
    pub max_results: usize,
    pub warnings: Vec<String>,
}

struct SearchLocation {
    root: PathBuf,
    cwd: PathBuf,
    exact_file: bool,
    glob: Option<String>,
}

/// Bind traversal to a canonical authorized directory before starting rg.
/// Keep wildcard filtering relative to that directory, with its original
/// anchoring. No wildcard expansion or extra search roots are introduced.
fn search_location(file_glob: Option<&str>, config: &AppConfig) -> Result<Option<SearchLocation>> {
    let project_root = crate::tools::scope::ensure_in_project_scope(&config.project_root, config)?;
    let Some(pattern) = file_glob else {
        return Ok(Some(SearchLocation {
            root: project_root.clone(),
            cwd: project_root,
            exact_file: false,
            glob: None,
        }));
    };
    let original_pattern = pattern;
    // The configured project-root spelling is a literal filesystem path,
    // even when its directory names contain glob metacharacters.
    let relative = if Path::new(pattern).is_absolute() {
        Path::new(pattern)
            .strip_prefix(&config.project_root)
            .or_else(|_| Path::new(pattern).strip_prefix(&project_root))
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    } else {
        None
    };
    let pattern = relative.as_deref().unwrap_or(pattern);
    let literal_end = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    let prefix_end = pattern[..literal_end]
        .rfind('/')
        .map_or(0, |index| index + 1);
    let prefix = if prefix_end == 0 {
        "."
    } else {
        &pattern[..prefix_end]
    };
    let suffix = &pattern[prefix_end..];
    // Dot segments in the literal prefix are resolved by the filesystem.
    // A parent component after a wildcard has no single provable target.
    anyhow::ensure!(
        !Path::new(suffix)
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir)),
        "search glob has an ambiguous parent traversal after a wildcard"
    );
    let requested_root = if Path::new(prefix).is_absolute() {
        PathBuf::from(prefix)
    } else {
        config.project_root.join(prefix)
    };
    if Path::new(original_pattern).is_absolute() && relative.is_none() {
        // Preserve the legacy empty-result behavior for ordinary absolute
        // globs outside the project. Equivalent project-root aliases remain
        // valid; allowed_paths does not activate additional absolute roots.
        let canonical = crate::tools::scope::canonicalize_target(&requested_root);
        if !canonical.is_some_and(|root| root.starts_with(&project_root)) {
            return Ok(None);
        }
    }
    let root = match crate::tools::scope::ensure_in_project_scope(&requested_root, config) {
        Ok(root) => root,
        Err(error) => {
            if literal_end != pattern.len() {
                return Err(error);
            }
            // An exact file may be individually allowed without authorizing
            // traversal of its parent or reading its neighboring files.
            let requested_file = if Path::new(pattern).is_absolute() {
                PathBuf::from(pattern)
            } else {
                config.project_root.join(pattern)
            };
            let file = crate::tools::scope::ensure_in_project_scope(&requested_file, config)?;
            anyhow::ensure!(
                file.is_file(),
                "authorized search target is not a regular file"
            );
            return Ok(Some(SearchLocation {
                root: file,
                cwd: project_root,
                exact_file: true,
                glob: None,
            }));
        }
    };
    if !root.is_dir() {
        return Ok(None);
    }
    let glob = if prefix_end == 0 {
        suffix.to_string()
    } else {
        format!("/{suffix}")
    };
    Ok(Some(SearchLocation {
        cwd: root.clone(),
        root,
        exact_file: false,
        glob: Some(glob),
    }))
}

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: "search_text".to_string(),
            description: "Regex search within files matching `file_glob`. Wraps `rg`. `file_glob` is required (e.g., '**/*.rs'). Use for text/pattern finding.".to_string(),
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
                        "description": "A glob pattern to filter which files are searched. This pattern must include a file extension or wildcard. Examples: 'src/**/*.rs', '**/*.toml', '**/*'."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Optional: Maximum number of matches to return. Default 200, hard cap 2000."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Optional: Number of matches to skip before collecting results. Use with `max_results` for pagination."
                    },
                    "response_budget_chars": {
                        "type": "integer",
                        "description": "Optional: Approximate maximum characters for the response (default 6000). Excess results are dropped with `truncated` and a resumable `next_offset`."
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
    config: &AppConfig,
) -> Result<Vec<(PathBuf, usize, String)>> {
    let result = search_text_with_options(
        search_pattern,
        file_glob,
        SearchTextOptions::default(),
        config,
    )?;
    Ok(result.rows)
}

fn normalize_options(options: SearchTextOptions) -> (usize, usize) {
    let max_results = options
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, HARD_MAX_RESULTS);
    let offset = options.offset.unwrap_or(0);
    (max_results, offset)
}

pub fn search_text_with_options(
    search_pattern: &str,
    file_glob: Option<&str>,
    options: SearchTextOptions,
    config: &AppConfig,
) -> Result<SearchTextResult> {
    let (max_results, offset) = normalize_options(options);
    let budget = options
        .response_budget_chars
        .unwrap_or(DEFAULT_SEARCH_BUDGET_CHARS)
        .max(200);

    let Some(location) = search_location(file_glob, config)? else {
        return Ok(SearchTextResult {
            rows: Vec::new(),
            truncated: false,
            next_offset: None,
            offset,
            max_results,
            warnings: Vec::new(),
        });
    };
    let mut cmd = Command::new("rg");
    // Ambient rg configuration can add traversal roots or enable symlink
    // following. The tool owns these arguments so authorization stays valid.
    cmd.arg("--no-config")
        .arg("--no-follow")
        .arg("--json")
        .arg("-n")
        .arg("-e")
        .arg(search_pattern);
    cmd.current_dir(&location.cwd);
    if let Some(glob) = &location.glob {
        // Keep rg filtering: expanding all paths would risk E2BIG.
        cmd.arg("--glob").arg(glob);
    }
    cmd.arg("--");
    if location.exact_file {
        cmd.arg(&location.root);
    } else {
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
    let mut match_index: usize = 0;
    let mut truncated = false;

    for line_res in reader.lines() {
        let line = line_res.context("failed to read ripgrep output")?;
        // Track bytes read from ripgrep and stop if exceeding the limit
        bytes_read = bytes_read.saturating_add(line.len());
        if bytes_read > MAX_OUTPUT_BYTES {
            // try to terminate the child process
            let _ = child.kill();
            truncated = true;
            break;
        }

        if let Ok(parsed) = serde_json::from_str::<RipgrepJson>(&line)
            && let RipgrepMessageType::Match = parsed.r#type
            && let (Some(path_text), Some(lines_text), Some(line_number)) =
                (parsed.data.path, parsed.data.lines, parsed.data.line_number)
        {
            let raw_path = PathBuf::from(path_text.text);
            let abs_path = if raw_path.is_absolute() {
                raw_path
            } else {
                location.cwd.join(raw_path)
            };
            // Refuse unexpected or retargeted paths before exposing match text.
            let authorized = crate::tools::scope::ensure_in_project_scope(&abs_path, config)
                .and_then(|path| {
                    if location.exact_file {
                        anyhow::ensure!(
                            path == location.root && path.is_file(),
                            "search result left its authorized exact file"
                        );
                    } else {
                        anyhow::ensure!(
                            path.starts_with(&location.root),
                            "search result left its authorized traversal root"
                        );
                    }
                    Ok(path)
                });
            let abs_path = match authorized {
                Ok(path) => path,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            match_index = match_index.saturating_add(1);
            if match_index <= offset {
                continue;
            }

            if results.len() >= max_results {
                truncated = true;
                let _ = child.kill();
                break;
            }

            results.push((abs_path, line_number, lines_text.text.trim().to_string()));
        }
    }

    // Ensure child process has exited
    let _ = child.wait();

    // Trim per-match text and enforce the response budget.
    let mut warnings = Vec::new();
    for (_, _, text) in results.iter_mut() {
        if text.chars().count() > MAX_MATCH_TEXT_CHARS {
            *text = format!(
                "{}...[line truncated]",
                crate::tools::budget::safe_take_chars(text, MAX_MATCH_TEXT_CHARS)
            );
        }
    }
    // Row cost = path + text + JSON overhead. Keep a running total so the
    // pop loop stays O(n).
    let row_cost = |row: &(PathBuf, usize, String)| -> usize {
        row.0.to_string_lossy().chars().count() + row.2.chars().count() + PER_RESULT_OVERHEAD
    };
    let mut total: usize = results.iter().map(row_cost).sum();
    let mut truncated = truncated || total > budget;
    while results.len() > 1 && total > budget {
        // Never pop the last row: an empty response with `truncated=true`
        // would leave the model no resumable pointer. If a single row exceeds
        // the budget, emit it anyway (already text-capped).
        total = total.saturating_sub(row_cost(results.last().expect("non-empty")));
        results.pop();
        truncated = true;
    }
    if truncated && !results.is_empty() {
        warnings.push(format!(
            "results limited to fit ~{} chars; re-run with `offset` (next_offset) for more",
            budget
        ));
    }

    let next_offset = if truncated && !results.is_empty() {
        Some(offset.saturating_add(results.len()))
    } else {
        None
    };

    Ok(SearchTextResult {
        rows: results,
        truncated,
        next_offset,
        offset,
        max_results,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use crate::config::AppConfig;
    use crate::tools::search_text::{SearchTextOptions, search_text, search_text_with_options};
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
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };
        let results = search_text("hello", Some(&file_glob), &config).unwrap();
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
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };
        let results = search_text("find me", Some(&file_glob), &config).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, root.join("a.txt"));
    }

    #[test]
    fn test_fs_search_no_match() {
        let root = create_temp_dir();
        fs::write(root.join("test.txt"), "some content").unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };
        let results = search_text("nonexistent", Some(&file_glob), &config).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_text_with_pagination() {
        let root = create_temp_dir();
        fs::write(root.join("test.txt"), "hit\nhit\nhit\nhit\n").unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };

        let page1 = search_text_with_options(
            "hit",
            Some(&file_glob),
            SearchTextOptions {
                max_results: Some(2),
                offset: Some(0),
                response_budget_chars: None,
            },
            &config,
        )
        .unwrap();
        assert_eq!(page1.rows.len(), 2);
        assert!(page1.truncated);
        assert_eq!(page1.next_offset, Some(2));

        let page2 = search_text_with_options(
            "hit",
            Some(&file_glob),
            SearchTextOptions {
                max_results: Some(2),
                offset: page1.next_offset,
                response_budget_chars: None,
            },
            &config,
        )
        .unwrap();
        assert_eq!(page2.rows.len(), 2);
    }

    #[test]
    fn test_search_text_budget_limits_rows() {
        let root = create_temp_dir();
        // 200 matches, each ~60 chars of text -> ~12k+ chars unbounded.
        let mut content = String::new();
        for i in 0..200 {
            content.push_str(&format!("needle in line {i} with some padding text\n"));
        }
        fs::write(root.join("data.txt"), content).unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };

        let result = search_text_with_options(
            "needle",
            Some(&file_glob),
            SearchTextOptions {
                max_results: Some(200),
                offset: Some(0),
                response_budget_chars: Some(3_000),
            },
            &config,
        )
        .unwrap();

        assert!(result.truncated);
        assert!(result.next_offset.is_some());
        assert!(!result.warnings.is_empty());
        let estimated: usize = result
            .rows
            .iter()
            .map(|(p, _, t)| p.to_string_lossy().len() + t.len() + 40)
            .sum();
        assert!(estimated <= 3_000, "estimated size too large: {estimated}");
    }

    #[test]
    fn test_search_text_single_oversized_row_still_returned() {
        let root = create_temp_dir();
        let long_line = format!("needle {}", "x".repeat(3_000));
        fs::write(root.join("big.txt"), long_line).unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };

        // Budget far smaller than the single (text-capped) row: the response
        // must still contain the row so the model gets a resumable result.
        let result = search_text_with_options(
            "needle",
            Some(&file_glob),
            SearchTextOptions {
                max_results: Some(10),
                offset: Some(0),
                response_budget_chars: Some(200),
            },
            &config,
        )
        .unwrap();

        assert_eq!(result.rows.len(), 1, "last row must survive the budget cut");
        assert!(result.rows[0].2.contains("[line truncated]"));
    }

    #[test]
    fn test_search_text_trims_long_lines() {
        let root = create_temp_dir();
        let long_line = format!("needle {}", "x".repeat(2_000));
        fs::write(root.join("min.txt"), long_line).unwrap();

        let root_str = root.to_str().unwrap();
        let file_glob = format!("{}/*.txt", root_str);
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };

        let result = search_text_with_options(
            "needle",
            Some(&file_glob),
            SearchTextOptions {
                max_results: Some(10),
                offset: Some(0),
                response_budget_chars: None,
            },
            &config,
        )
        .unwrap();

        assert_eq!(result.rows.len(), 1);
        assert!(result.rows[0].2.chars().count() <= 520);
        assert!(result.rows[0].2.contains("[line truncated]"));
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    const OUTSIDE_MARKER: &str = "DGC_HARMLESS_OUTSIDE_SCOPE_MARKER";
    struct Fixture {
        _temp: tempfile::TempDir,
        project: PathBuf,
        outside: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path().canonicalize().unwrap();
            let project = base.join("project");
            let outside = base.join("outside");
            std::fs::create_dir(&project).unwrap();
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("secret.txt"), OUTSIDE_MARKER).unwrap();
            std::fs::write(
                project.join("inside.txt"),
                "inside marker\ninside marker\ninside marker\n",
            )
            .unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&outside, project.join("link")).unwrap();
            Self {
                _temp: temp,
                project,
                outside,
            }
        }
        fn config(&self) -> AppConfig {
            AppConfig {
                project_root: self.project.clone(),
                ..Default::default()
            }
        }
        fn escapes(&self) -> Vec<String> {
            let mut patterns = vec![
                "../outside/secret.txt".into(),
                self.project
                    .join("../outside/secret.txt")
                    .display()
                    .to_string(),
            ];
            #[cfg(unix)]
            patterns.push("link/secret.txt".into());
            patterns
        }
    }
    #[test]
    fn search_scope_rejects_traversal_and_external_symlink_before_search() {
        let fixture = Fixture::new();
        for pattern in fixture.escapes() {
            let result = search_text(OUTSIDE_MARKER, Some(&pattern), &fixture.config());
            assert!(
                result.is_err(),
                "outside glob accepted: {pattern}: {result:?}"
            );
            assert!(!result.unwrap_err().to_string().contains(OUTSIDE_MARKER));
        }
    }
    #[tokio::test]
    async fn search_scope_dispatch_rejects_all_escape_spellings() -> anyhow::Result<()> {
        let fixture = Fixture::new();
        let fs =
            crate::tools::FsTools::new(Arc::new(RwLock::new(None)), Arc::new(fixture.config()));
        let runtime =
            crate::llm::tool_runtime::ToolRuntime::build(&fs, None, "fixture", None).await?;
        runtime.tool_catalog.activate(&["search_text".into()]).await;
        for pattern in fixture.escapes() {
            let tc = crate::llm::types::ToolCall {
                id: Some("search".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "search_text".into(),
                    arguments: json!({"search_pattern":OUTSIDE_MARKER,"file_glob":pattern})
                        .to_string(),
                },
            };
            let result = crate::llm::tool_execution::dispatch_tool_call(&runtime, &tc).await;
            assert!(
                result.is_err(),
                "dispatch returned outside content: {result:?}"
            );
            assert!(!result.unwrap_err().to_string().contains(OUTSIDE_MARKER));
        }
        Ok(())
    }
    #[tokio::test]
    async fn search_scope_mcp_rejects_all_escape_spellings() {
        use rmcp::handler::server::wrapper::Parameters;
        let fixture = Fixture::new();
        let service = crate::mcp::service::DogeMcpService::new(Arc::new(
            crate::mcp::service::McpServiceState::new(
                Arc::new(fixture.config()),
                Arc::new(RwLock::new(None)),
            ),
        ));
        for pattern in fixture.escapes() {
            let result = service
                .search_text(Parameters(crate::mcp::service::SearchTextParams {
                    search_pattern: OUTSIDE_MARKER.into(),
                    file_glob: Some(pattern),
                    max_results: None,
                    offset: None,
                }))
                .unwrap();
            assert_eq!(
                result.is_error,
                Some(true),
                "MCP returned outside content: {result:?}"
            );
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains(OUTSIDE_MARKER)
            );
        }
    }
    #[test]
    fn search_scope_preserves_globs_exact_names_dot_segments_and_pagination() {
        let fixture = Fixture::new();
        let config = fixture.config();
        std::fs::create_dir_all(fixture.project.join("src/nested")).unwrap();
        std::fs::write(fixture.project.join("src/main.rs"), "inside marker").unwrap();
        std::fs::write(fixture.project.join("src/nested/main.rs"), "inside marker").unwrap();
        for (glob, count) in [
            ("*.txt", 3),
            ("**/*.txt", 3),
            ("inside.txt", 3),
            ("./inside.txt", 3),
            ("src/../inside.txt", 3),
            ("src/**/*.rs", 2),
            ("src/*.rs", 1),
            ("src/nested/main.rs", 1),
            ("missing/*.txt", 0),
        ] {
            let results = search_text("inside marker", Some(glob), &config).unwrap();
            assert_eq!(results.len(), count, "glob {glob}: {results:?}");
            assert!(
                results
                    .iter()
                    .all(|(path, _, _)| path.starts_with(&fixture.project))
            );
        }
        let results = search_text("inside marker", None, &config).unwrap();
        assert_eq!(results.len(), 5);
        let first = search_text_with_options(
            "inside marker",
            Some("inside.txt"),
            SearchTextOptions {
                max_results: Some(2),
                ..Default::default()
            },
            &config,
        )
        .unwrap();
        assert_eq!(first.rows.len(), 2);
        assert_eq!(first.next_offset, Some(2));
        assert!(first.truncated);
        let last = search_text_with_options(
            "inside marker",
            Some("inside.txt"),
            SearchTextOptions {
                offset: first.next_offset,
                ..Default::default()
            },
            &config,
        )
        .unwrap();
        assert_eq!(last.rows.len(), 1);
        assert!(!last.truncated);
        assert_eq!(last.next_offset, None);
    }
    #[test]
    fn search_scope_preserves_existing_explicit_permissions_without_adding_roots() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config.allowed_paths.push(fixture.outside.clone());
        for pattern in fixture.escapes() {
            let results = search_text(OUTSIDE_MARKER, Some(&pattern), &config).unwrap();
            assert_eq!(results.len(), 1, "explicit permission: {pattern}");
            assert_eq!(results[0].0, fixture.outside.join("secret.txt"));
        }
        let absolute_outside = fixture.outside.join("secret.txt").display().to_string();
        assert!(
            search_text(OUTSIDE_MARKER, Some(&absolute_outside), &config)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_text(OUTSIDE_MARKER, None, &config)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    #[cfg(unix)]
    fn search_scope_preserves_root_alias_and_internal_symlink_prefix() {
        let fixture = Fixture::new();
        let alias = fixture.project.with_file_name("root-alias");
        std::os::unix::fs::symlink(&fixture.project, &alias).unwrap();
        let mut config = fixture.config();
        config.project_root = alias.clone();
        for pattern in [
            "*.txt".into(),
            alias.join("*.txt").display().to_string(),
            fixture.project.join("*.txt").display().to_string(),
        ] {
            assert_eq!(
                search_text("inside marker", Some(&pattern), &config)
                    .unwrap()
                    .len(),
                3
            );
        }
        std::fs::create_dir(fixture.project.join("src")).unwrap();
        std::fs::write(fixture.project.join("src/inside.rs"), "inside marker").unwrap();
        std::os::unix::fs::symlink(
            fixture.project.join("src"),
            fixture.project.join("internal-link"),
        )
        .unwrap();
        let results = search_text("inside marker", Some("internal-link/*.rs"), &config).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, fixture.project.join("src/inside.rs"));
        std::os::unix::fs::symlink(
            fixture.outside.join("secret.txt"),
            fixture.project.join("outside-file.txt"),
        )
        .unwrap();
        assert!(
            search_text(OUTSIDE_MARKER, Some("*.txt"), &config)
                .unwrap()
                .is_empty()
        );
        assert!(
            search_text(OUTSIDE_MARKER, Some("outside-file.txt"), &config)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn search_scope_keeps_literal_project_root_metacharacters() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("project[1]");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("inside.txt"), "inside marker").unwrap();
        let config = AppConfig {
            project_root: root.clone(),
            ..Default::default()
        };
        let glob = root.join("*.txt").display().to_string();
        assert_eq!(
            search_text("inside marker", Some(&glob), &config)
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn search_scope_dispatch_and_mcp_keep_valid_results_and_counts() -> anyhow::Result<()> {
        use rmcp::handler::server::wrapper::Parameters;
        let fixture = Fixture::new();
        let cfg = Arc::new(fixture.config());
        let fs = crate::tools::FsTools::new(Arc::new(RwLock::new(None)), cfg.clone());
        let runtime =
            crate::llm::tool_runtime::ToolRuntime::build(&fs, None, "fixture", None).await?;
        runtime.tool_catalog.activate(&["search_text".into()]).await;
        let service = crate::mcp::service::DogeMcpService::new(Arc::new(
            crate::mcp::service::McpServiceState::new(cfg, Arc::new(RwLock::new(None))),
        ));
        for (needle, count) in [("inside marker", 2), ("absent marker", 0)] {
            let tc = crate::llm::types::ToolCall {
                id: Some("search".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "search_text".into(),
                    arguments:
                        json!({"search_pattern":needle,"file_glob":"inside.txt","max_results":2})
                            .to_string(),
                },
            };
            let result = crate::llm::tool_execution::dispatch_tool_call(&runtime, &tc).await?;
            assert!(result.is_success);
            assert_eq!(result.value["meta"]["returned"], count);
            assert_eq!(result.value["meta"]["truncated"], count > 0);
            let result = service
                .search_text(Parameters(crate::mcp::service::SearchTextParams {
                    search_pattern: needle.into(),
                    file_glob: Some("inside.txt".into()),
                    max_results: Some(2),
                    offset: None,
                }))
                .unwrap();
            assert_eq!(result.is_error, Some(false));
            let serialized = serde_json::to_string(&result).unwrap();
            assert!(!serialized.contains(OUTSIDE_MARKER));
            assert_eq!(serialized.contains("[truncated]"), count > 0);
        }
        Ok(())
    }
    #[test]
    fn search_scope_preserves_individually_allowed_file_without_reading_neighbors() {
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config
            .allowed_paths
            .push(fixture.outside.join("secret.txt"));
        std::fs::write(fixture.outside.join("neighbor.txt"), OUTSIDE_MARKER).unwrap();
        for pattern in fixture.escapes() {
            let results = search_text(OUTSIDE_MARKER, Some(&pattern), &config).unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].0, fixture.outside.join("secret.txt"));
        }
        for pattern in ["../outside/*.txt", "../outside/neighbor.txt"] {
            assert!(search_text(OUTSIDE_MARKER, Some(pattern), &config).is_err());
        }
        let absolute_outside = fixture.outside.join("secret.txt").display().to_string();
        assert!(
            search_text(OUTSIDE_MARKER, Some(&absolute_outside), &config)
                .unwrap()
                .is_empty()
        );
    }
}
