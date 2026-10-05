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
    Summary,
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

enum RecordEvent<'a> {
    Record(&'a [u8]),
    Eof,
    Limit,
}

/// Fixed-size I/O buffering and a capped complete-record buffer. Newline
/// bytes count toward the total cap; never parse a record cut by the cap.
struct Records<R> {
    reader: std::io::BufReader<RetryReads<R>>,
    record: Vec<u8>,
    total: usize,
    cap: usize,
}
struct RetryReads<R>(R);
impl<R: std::io::Read> std::io::Read for RetryReads<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.0.read(bytes) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }
}
impl<R: std::io::Read> Records<R> {
    fn new(reader: R, cap: usize) -> Self {
        Self {
            reader: std::io::BufReader::with_capacity(8192, RetryReads(reader)),
            record: Vec::new(),
            total: 0,
            cap,
        }
    }
    fn next(&mut self) -> std::io::Result<RecordEvent<'_>> {
        use std::io::BufRead;
        self.record.clear();
        loop {
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                ensure_complete_record(!self.record.is_empty())?;
                return Ok(RecordEvent::Eof);
            }
            let end = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1);
            let count = end.unwrap_or(available.len());
            let remaining = self.cap.saturating_sub(self.total);
            if count > remaining {
                return Ok(RecordEvent::Limit);
            }
            let content_count = count - usize::from(end.is_some());
            self.record
                .try_reserve_exact(content_count)
                .map_err(std::io::Error::other)?;
            self.record.extend_from_slice(&available[..content_count]);
            self.reader.consume(count);
            self.total += count;
            if end.is_some() {
                return Ok(RecordEvent::Record(&self.record));
            }
        }
    }
}
fn ensure_complete_record(incomplete: bool) -> std::io::Result<()> {
    if incomplete {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "incomplete ripgrep JSON record at EOF",
        ));
    }
    Ok(())
}
fn search_error(message: impl std::fmt::Display) -> anyhow::Error {
    // The shared 512-scalar prefix also fits the serialized tool envelope
    // when diagnostic control characters expand to six-byte JSON escapes.
    crate::tools::budget::bounded_read_error(message)
}

fn check_search_exit<T>(output: &crate::execution::runner::ManagedStreamOutput<T>) -> Result<()> {
    #[cfg(unix)]
    let killed = {
        use std::os::unix::process::ExitStatusExt;
        output.status.signal() == Some(libc::SIGKILL)
    };
    #[cfg(not(unix))]
    let killed = output.status.code() == Some(1);
    let intentional_kill = output.stop == crate::execution::runner::StreamStop::Limit
        && output.kill_requested
        && killed;
    anyhow::ensure!(
        matches!(output.status.code(), Some(0 | 1)) || intentional_kill,
        "{}",
        search_error(format!(
            "ripgrep failed with {}: {}",
            output.status, output.stderr
        ))
    );
    Ok(())
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
    search_with_program(
        search_pattern,
        file_glob,
        options,
        config,
        std::ffi::OsStr::new("rg"),
    )
}

fn search_with_program(
    search_pattern: &str,
    file_glob: Option<&str>,
    options: SearchTextOptions,
    config: &AppConfig,
    program: &std::ffi::OsStr,
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
    let mut cmd = Command::new(program);
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
    let streamed = crate::execution::runner::run_managed_stream(cmd, |stdout| {
        use crate::execution::runner::StreamStop;
        let mut records = Records::new(stdout, MAX_OUTPUT_BYTES);
        let mut results = Vec::new();
        let mut match_index = 0usize;
        loop {
            let record = match records.next().context("failed to read ripgrep output")? {
                RecordEvent::Eof => return Ok(((results, false), StreamStop::Eof)),
                RecordEvent::Limit => {
                    anyhow::ensure!(!results.is_empty(),
                        "ripgrep output exceeded the 1048576-byte limit before a requested match could be returned; narrow the search pattern or file_glob (no resumable next_offset)");
                    return Ok(((results, true), StreamStop::Limit));
                }
                RecordEvent::Record(record) => record,
            };
            let parsed: RipgrepJson = serde_json::from_slice(record)
                .map_err(|error| search_error(format!("invalid ripgrep JSON record: {error}")))?;
            if !matches!(parsed.r#type, RipgrepMessageType::Match) { continue; }
            let (path_text, lines_text, line_number) = match (parsed.data.path, parsed.data.lines, parsed.data.line_number) {
                (Some(path), Some(lines), Some(number)) => (path, lines, number),
                _ => anyhow::bail!("incomplete ripgrep match fields"),
            };
            let raw_path = PathBuf::from(path_text.text);
            let abs_path = if raw_path.is_absolute() { raw_path } else { location.cwd.join(raw_path) };
            let abs_path = crate::tools::scope::ensure_in_project_scope(&abs_path, config)?;
            if location.exact_file {
                anyhow::ensure!(abs_path == location.root && abs_path.is_file(), "search result left its authorized exact file");
            } else {
                anyhow::ensure!(abs_path.starts_with(&location.root), "search result left its authorized traversal root");
            }
            // Authorization precedes skipping, budgeting and exposing match text.
            match_index = match_index.saturating_add(1);
            if match_index <= offset { continue; }
            if results.len() >= max_results { return Ok(((results, false), StreamStop::Limit)); }
            results.push((abs_path, line_number, lines_text.text.trim().to_string()));
        }
    }).map_err(search_error)?;
    check_search_exit(&streamed)?;
    let (mut results, byte_limited) = streamed.value;
    let truncated = streamed.stop == crate::execution::runner::StreamStop::Limit;

    // Trim per-match text and enforce the response budget.
    let mut warnings = Vec::new();
    if byte_limited {
        warnings.push(
            "ripgrep stdout reached the byte capture limit; only complete match records returned"
                .into(),
        );
    }
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

#[cfg(test)]
mod process_tests {
    use super::*;
    use std::io::{self, Read};

    #[test]
    fn search_records_count_newlines_and_keep_exact_caps() {
        let mut records = Records::new(b"a\n\xc3\xa9\n".as_slice(), 5);
        assert!(matches!(records.next().unwrap(), RecordEvent::Record(b"a")));
        assert!(matches!(
            records.next().unwrap(),
            RecordEvent::Record(b"\xc3\xa9")
        ));
        assert!(matches!(records.next().unwrap(), RecordEvent::Eof));
        assert_eq!(records.total, 5);
        for bytes in [b"a\n\xc3\xa9\nx".as_slice(), b"a\n\xc3\xa9x\n"] {
            let mut records = Records::new(bytes, 5);
            assert!(matches!(records.next().unwrap(), RecordEvent::Record(_)));
            if matches!(records.next().unwrap(), RecordEvent::Record(_)) {
                assert!(matches!(records.next().unwrap(), RecordEvent::Limit));
            }
        }
        for bytes in [b"{}".as_slice(), b"a\n\xc3"] {
            let mut records = Records::new(bytes, 10);
            if bytes.starts_with(b"a") {
                records.next().unwrap();
            }
            assert_eq!(
                records.next().err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn search_records_generated_unterminated_input_stays_bounded_and_propagates_io() {
        struct Generated {
            left: usize,
        }
        impl Read for Generated {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let count = self.left.min(bytes.len());
                bytes[..count].fill(b'x');
                self.left -= count;
                Ok(count)
            }
        }
        let mut records = Records::new(
            Generated {
                left: 128 * 1024 * 1024,
            },
            MAX_OUTPUT_BYTES,
        );
        assert!(matches!(records.next().unwrap(), RecordEvent::Limit));
        assert!(records.record.len() <= MAX_OUTPUT_BYTES);
        assert!(records.record.capacity() <= MAX_OUTPUT_BYTES);
        assert!(records.total <= MAX_OUTPUT_BYTES);
        struct Failure;
        impl Read for Failure {
            fn read(&mut self, _bytes: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("fixture failure"))
            }
        }
        assert!(
            Records::new(Failure, 10)
                .next()
                .err()
                .unwrap()
                .to_string()
                .contains("fixture failure")
        );
    }

    #[tokio::test]
    async fn search_process_direct_dispatch_mcp_distinguish_regex_error_and_no_match() -> Result<()>
    {
        use crate::llm::tool_execution::dispatch_tool_call;
        use crate::llm::tool_runtime::ToolRuntime;
        use crate::llm::types::{ToolCall, ToolCallFunction};
        use crate::mcp::service::{DogeMcpService, McpServiceState, SearchTextParams};
        use rmcp::handler::server::wrapper::Parameters;
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("file.txt"), "inside marker\n")?;
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_owned(),
            ..Default::default()
        });
        let fs = crate::tools::FsTools::new(Arc::new(RwLock::new(None)), config.clone());
        let runtime = ToolRuntime::build(&fs, None, "fixture", None).await?;
        runtime.tool_catalog.activate(&["search_text".into()]).await;
        let service = DogeMcpService::new(Arc::new(McpServiceState::new(
            config.clone(),
            Arc::new(RwLock::new(None)),
        )));
        for (pattern, success) in [("[", false), ("ABSENT_FIXTURE_PATTERN", true)] {
            let direct = search_text(pattern, Some("*.txt"), &config);
            assert_eq!(direct.is_ok(), success);
            if success {
                assert!(direct?.is_empty());
            } else {
                assert!(direct.unwrap_err().to_string().contains("ripgrep failed"));
            }
            let call = ToolCall {
                id: Some("search-error".into()),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: "search_text".into(),
                    arguments: json!({"search_pattern":pattern,"file_glob":"*.txt"}).to_string(),
                },
            };
            assert_eq!(dispatch_tool_call(&runtime, &call).await.is_ok(), success);
            let mcp = service.search_text(Parameters(SearchTextParams {
                search_pattern: pattern.into(),
                file_glob: Some("*.txt".into()),
                max_results: None,
                offset: None,
            }))?;
            assert_eq!(mcp.is_error == Some(true), !success);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn fake_search(script: &str, options: SearchTextOptions) -> Result<SearchTextResult> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("file.txt"), "fixture")?;
        let executable = dir.path().join("fixture-rg");
        std::fs::write(&executable, format!("#!/bin/sh\n{script}\n"))?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let config = AppConfig {
            project_root: dir.path().to_owned(),
            ..Default::default()
        };
        search_with_program(
            "fixture",
            Some("*.txt"),
            options,
            &config,
            executable.as_os_str(),
        )
    }
    #[cfg(unix)]
    pub(super) const ROW: &str = r#"printf '%s\n' '{"type":"match","data":{"path":{"text":"file.txt"},"lines":{"text":"fixture\n"},"line_number":1}}'"#;

    #[cfg(unix)]
    #[test]
    fn search_process_drains_large_stderr_and_reports_partial_failure_and_signal() {
        // A fixture-owned watchdog prevents a broken drain implementation hanging the suite.
        let watchdog = "(sleep 3; kill -KILL $$) >/dev/null 2>&1 &";
        let result = fake_search(
            &format!("{watchdog}\ndd if=/dev/zero bs=65536 count=3 >&2 2>/dev/null\n{ROW}\nexit 0"),
            Default::default(),
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        let error = fake_search(
            &format!("{ROW}\nprintf 'fixture diagnostic' >&2\nexit 2"),
            Default::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("fixture diagnostic") && error.contains("ripgrep failed"));
        assert!(error.len() < 3000);
        let controls = fake_search(
            "dd if=/dev/zero bs=65536 count=3 >&2 2>/dev/null; exit 2",
            Default::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            serde_json::json!({"error":controls})
                .to_string()
                .chars()
                .count()
                < 8000
        );
        assert!(
            fake_search("kill -TERM $$", Default::default())
                .unwrap_err()
                .to_string()
                .contains("ripgrep failed")
        );
        assert!(
            fake_search("printf '{partial'", Default::default())
                .unwrap_err()
                .to_string()
                .contains("read ripgrep output")
        );
    }

    #[cfg(unix)]
    #[test]
    fn search_process_caps_explain_unreturned_match_and_preserve_returned_rows() {
        let huge = "dd if=/dev/zero bs=65536 count=20 2>/dev/null";
        let error = fake_search(huge, Default::default())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("before a requested match")
                && error.contains("no resumable next_offset")
        );
        let result = fake_search(&format!("{ROW}\n{huge}"), Default::default()).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert!(result.truncated);
        assert_eq!(result.next_offset, Some(1));
        let error = fake_search(
            &format!("{ROW}\n{huge}"),
            SearchTextOptions {
                offset: Some(1),
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("before a requested match"));
        let result = fake_search(
            &format!("{ROW}\n{ROW}\nsleep 3"),
            SearchTextOptions {
                max_results: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.truncated);
        assert_eq!(result.rows.len(), 1);
    }
}

#[cfg(all(test, unix))]
mod process_cleanup_tests {
    use super::*;
    #[test]
    fn search_process_error_and_early_stop_reap_child_and_terminate_descendants() {
        use std::os::unix::fs::PermissionsExt;
        for failure in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let escaped = dir.path().join("descendant-survived");
            let executable = dir.path().join("fixture-rg");
            std::fs::write(dir.path().join("file.txt"), "fixture").unwrap();
            let row = process_tests::ROW;
            let body = if failure {
                "printf 'invalid-json\\n'".to_string()
            } else {
                format!("{row}\n{row}")
            };
            std::fs::write(&executable, format!("#!/bin/sh\nprintf '%s' $$ > '{}'\n(sleep 1; printf survived > '{}') >/dev/null 2>&1 &\n{body}\nsleep 3\n", pid_file.display(), escaped.display())).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let config = AppConfig {
                project_root: dir.path().to_owned(),
                ..Default::default()
            };
            let result = search_with_program(
                "fixture",
                Some("*.txt"),
                SearchTextOptions {
                    max_results: Some(1),
                    ..Default::default()
                },
                &config,
                executable.as_os_str(),
            );
            assert_eq!(result.is_err(), failure);
            let pid: u32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
            assert!(
                !crate::execution::is_process_alive(pid),
                "direct child was not reaped"
            );
            std::thread::sleep(std::time::Duration::from_millis(1200));
            assert!(!escaped.exists(), "fixture descendant survived cleanup");
        }
    }
}

#[cfg(all(test, unix))]
mod limit_status_tests {
    use super::*;
    #[test]
    fn search_process_limit_preserves_natural_exit_error_and_signal() {
        use crate::execution::runner::{StreamStop, run_managed_stream};
        for script in ["printf 'fixture diagnostic' >&2; exit 2", "kill -TERM $$"] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            let output = run_managed_stream(command, |stdout| {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes)?;
                // EOF from this fixture comes from process exit, then model a cap decision.
                Ok(((), StreamStop::Limit))
            })
            .unwrap();
            assert!(
                check_search_exit(&output)
                    .unwrap_err()
                    .to_string()
                    .contains("ripgrep failed")
            );
        }
    }
}

#[cfg(test)]
mod record_boundary_tests {
    use super::*;
    #[test]
    fn search_records_utf8_split_at_io_boundary_and_partial_caps() {
        struct Short<R> {
            reader: R,
            size: usize,
        }
        impl<R: std::io::Read> std::io::Read for Short<R> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let len = bytes.len().min(self.size);
                self.reader.read(&mut bytes[..len])
            }
        }
        let message = json!({"type":"match","data":{"path":{"text":"file.txt"},"lines":{"text":"あ😀\n"},"line_number":1}}).to_string();
        let data = format!("{message}\n");
        for size in [1, 2, 3, 8192] {
            let mut reader = Records::new(
                Short {
                    reader: data.as_bytes(),
                    size,
                },
                data.len(),
            );
            let RecordEvent::Record(record) = reader.next().unwrap() else {
                panic!("missing complete record")
            };
            let parsed: RipgrepJson = serde_json::from_slice(record).unwrap();
            assert_eq!(parsed.data.lines.unwrap().text, "あ😀\n");
            assert!(matches!(reader.next().unwrap(), RecordEvent::Eof));
            let mut reader = Records::new(
                Short {
                    reader: data.as_bytes(),
                    size,
                },
                data.len() - 1,
            );
            assert!(matches!(reader.next().unwrap(), RecordEvent::Limit));
        }
        for (bytes, expected_second_record) in [
            (b"a\nb\nx".as_slice(), true),
            (b"a\nbx\n".as_slice(), false),
        ] {
            let mut reader = Records::new(bytes, 4);
            assert!(matches!(reader.next().unwrap(), RecordEvent::Record(b"a")));
            if expected_second_record {
                assert!(matches!(reader.next().unwrap(), RecordEvent::Record(b"b")));
            }
            assert!(matches!(reader.next().unwrap(), RecordEvent::Limit));
        }
    }
}
