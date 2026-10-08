use std::collections::BTreeSet;
use std::path::Path;

use super::types::{CandidateTest, TestConfidence};

/// Language tag for a file extension.
fn language_for_path(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    // Strip query-like suffixes? Paths are plain relative paths.
    if lower.ends_with(".rs") {
        Some("rust")
    } else if lower.ends_with(".go") {
        Some("go")
    } else if lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cjs")
    {
        Some("typescript")
    } else if lower.ends_with(".py") {
        Some("python")
    } else {
        None
    }
}

fn file_name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map(|i| &path[..i]).unwrap_or("")
}

fn stem_of(file_name: &str) -> &str {
    file_name.split('.').next().unwrap_or(file_name)
}

/// Whether the path itself is a test file (explicit evidence).
fn is_test_file(path: &str, language: &str) -> bool {
    let name = file_name_of(path);
    match language {
        "rust" => {
            // Integration tests live under tests/; unit tests live inline
            // (detected via content markers below). A file named
            // `*_test.rs` is also treated as a test file.
            path.starts_with("tests/")
                || path.contains("/tests/")
                || name.ends_with("_test.rs")
                || name.ends_with("test.rs")
        }
        "go" => name.ends_with("_test.go"),
        "typescript" => {
            name.ends_with(".test.ts")
                || name.ends_with(".spec.ts")
                || name.ends_with(".test.tsx")
                || name.ends_with(".spec.tsx")
                || name.ends_with(".test.js")
                || name.ends_with(".spec.js")
                || path.contains("/__tests__/")
                || path.starts_with("__tests__/")
        }
        "python" => {
            name.starts_with("test_") && name.ends_with(".py")
                || name.ends_with("_test.py")
                || path.starts_with("tests/")
                || path.contains("/tests/")
        }
        _ => false,
    }
}

/// Whether file content carries an inline test marker (one read per file).
fn content_has_test_marker(language: &str, content: &str) -> bool {
    // Bounded prefix scan: markers live near the top or inline; scanning
    // the first 64 KiB is enough and keeps RAM flat.
    let head: String = content.chars().take(65_536).collect();
    match language {
        "rust" => {
            head.contains("#[test]")
                || head.contains("#[tokio::test]")
                || head.contains("#[cfg(test)]")
        }
        "go" => head.contains("func Test"),
        "typescript" => {
            head.contains("describe(") || head.contains(" it(") || head.contains("test(")
        }
        "python" => head.contains("def test_") || head.contains("import pytest"),
        _ => false,
    }
}

/// Candidate sibling test paths for one source file (existence-checked).
fn sibling_candidates(project_root: &Path, path: &str, language: &str) -> Vec<String> {
    let mut out = Vec::new();
    let dir = dir_of(path);
    let name = file_name_of(path);
    let stem = stem_of(name);
    let mut push_if_exists = |rel: String| {
        if project_root.join(&rel).is_file() {
            out.push(rel);
        }
    };
    match language {
        "go" => {
            // Package-level: any *_test.go in the same directory.
            if let Ok(entries) = std::fs::read_dir(project_root.join(dir)) {
                let mut names: Vec<String> = Vec::new();
                for entry in entries.flatten() {
                    if let Ok(ft) = entry.file_type()
                        && ft.is_file()
                    {
                        let n = entry.file_name().to_string_lossy().to_string();
                        if n.ends_with("_test.go") {
                            names.push(n);
                        }
                    }
                }
                names.sort();
                // Bound the package scan so a huge package cannot blow the budget.
                for n in names.into_iter().take(10) {
                    if dir.is_empty() {
                        push_if_exists(n);
                    } else {
                        push_if_exists(format!("{dir}/{n}"));
                    }
                }
            }
        }
        "typescript" => {
            let base_no_ext = name
                .strip_suffix(".tsx")
                .or_else(|| name.strip_suffix(".ts"))
                .or_else(|| name.strip_suffix(".jsx"))
                .or_else(|| name.strip_suffix(".js"))
                .or_else(|| name.strip_suffix(".mjs"))
                .or_else(|| name.strip_suffix(".cjs"))
                .unwrap_or(stem);
            let ext = if name.ends_with(".tsx") {
                "tsx"
            } else if name.ends_with(".jsx") {
                "jsx"
            } else if name.ends_with(".js") {
                "js"
            } else {
                "ts"
            };
            for candidate in [
                format!("{dir}/{base_no_ext}.test.{ext}"),
                format!("{dir}/{base_no_ext}.spec.{ext}"),
            ] {
                let cleaned = candidate.trim_start_matches('/').to_string();
                push_if_exists(cleaned);
            }
            // __tests__ mirror.
            if !dir.is_empty() {
                push_if_exists(format!("{dir}/__tests__/{name}"));
            } else {
                push_if_exists(format!("__tests__/{name}"));
            }
        }
        "python" => {
            for candidate in [
                format!("{dir}/test_{stem}.py"),
                format!("{dir}/{stem}_test.py"),
                format!("tests/test_{stem}.py"),
            ] {
                let cleaned = candidate.trim_start_matches('/').to_string();
                push_if_exists(cleaned);
            }
        }
        "rust" => {
            // Integration tests are not derivable from the source name
            // alone; only report them when the changed file itself is a
            // test (handled above). Sibling guessing would invent coverage.
        }
        _ => {}
    }
    out.sort();
    out.dedup();
    out
}

/// Discover candidate test files (read-only, conservative, deterministic).
///
/// - `Exact`: the changed/impacted file itself is a test (definition in
///   the file or a reliable test-path convention).
/// - `Likely`: a sibling test file exists (package/dir association).
/// - Filename similarity alone never yields `Exact`.
/// - Returns empty when no evidence exists; the planner then falls back
///   to project-level verification with an `Unknown` coverage reason.
pub fn discover_candidate_tests(
    project_root: &Path,
    changed_files: &[String],
    impacted_files: &[String],
    include_tests: bool,
) -> (Vec<CandidateTest>, Vec<String>) {
    let mut warnings = Vec::new();
    if !include_tests {
        return (vec![], warnings);
    }
    let mut relevant: BTreeSet<String> = BTreeSet::new();
    for p in changed_files.iter().chain(impacted_files.iter()) {
        relevant.insert(p.clone());
    }
    let mut candidates: Vec<CandidateTest> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for path in relevant.iter() {
        let Some(language) = language_for_path(path) else {
            continue;
        };
        if is_test_file(path, language) {
            if seen.insert(path.clone()) {
                candidates.push(CandidateTest {
                    path: path.clone(),
                    confidence: TestConfidence::Exact,
                    language: language.to_string(),
                });
            }
            continue;
        }
        // Inline unit tests: the source file itself carries test markers.
        // This is explicit evidence, so the file is an `Exact` candidate.
        // Oversized files are skipped (same 1 MiB bound as stale detection)
        // so one generated artifact cannot blow memory.
        let abs = project_root.join(path);
        let readable = std::fs::metadata(&abs).is_ok_and(|m| m.is_file() && m.len() <= 1_048_576);
        if readable
            && let Ok(content) = std::fs::read_to_string(&abs)
            && content_has_test_marker(language, &content)
            && seen.insert(path.clone())
        {
            candidates.push(CandidateTest {
                path: path.clone(),
                confidence: TestConfidence::Exact,
                language: language.to_string(),
            });
            continue;
        }
        // Sibling association: `Likely`, never `Exact`.
        for sibling in sibling_candidates(project_root, path, language) {
            if seen.insert(sibling.clone()) {
                candidates.push(CandidateTest {
                    path: sibling,
                    confidence: TestConfidence::Likely,
                    language: language.to_string(),
                });
            }
        }
    }
    if candidates.is_empty() {
        warnings.push(
            "No test candidates with explicit evidence were found; static relationships are not runtime coverage evidence. Broader verification is recommended."
                .to_string(),
        );
    }
    // TypeScript without a test script: surface the gap explicitly.
    if relevant
        .iter()
        .any(|p| language_for_path(p) == Some("typescript"))
    {
        let pkg = project_root.join("package.json");
        match std::fs::read_to_string(&pkg) {
            Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(value) => {
                    let has_test = value
                        .get("scripts")
                        .and_then(|s| s.as_object())
                        .is_some_and(|scripts| {
                            scripts.contains_key("test")
                                || scripts.contains_key("test:unit")
                                || scripts.contains_key("test:all")
                        });
                    if !has_test {
                        warnings.push(
                            "package.json has no test script; TypeScript verification has no configured command."
                                .to_string(),
                        );
                    }
                }
                Err(_) => warnings.push(
                    "package.json could not be parsed; TypeScript test command is unknown."
                        .to_string(),
                ),
            },
            Err(_) => {
                // No package.json: source-only TS files; not a warning by itself.
            }
        }
    }
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    (candidates, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(abs, content).expect("write");
    }

    #[test]
    fn rust_unit_test_in_changed_file_is_exact() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "src/lib.rs",
            "#[cfg(test)]\nmod tests {\n#[test]\nfn it() {}\n}\n",
        );
        let (candidates, _) =
            discover_candidate_tests(dir.path(), &["src/lib.rs".to_string()], &[], true);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confidence, TestConfidence::Exact);
        assert_eq!(candidates[0].language, "rust");
    }

    #[test]
    fn rust_integration_test_path_is_exact() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "tests/api.rs", "#[test]\nfn api() {}\n");
        let (candidates, _) =
            discover_candidate_tests(dir.path(), &["tests/api.rs".to_string()], &[], true);
        assert_eq!(candidates[0].confidence, TestConfidence::Exact);
    }

    #[test]
    fn rust_without_markers_has_no_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "src/lib.rs", "pub fn foo() {}\n");
        let (candidates, warnings) =
            discover_candidate_tests(dir.path(), &["src/lib.rs".to_string()], &[], true);
        assert!(candidates.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn go_package_test_is_likely() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "internal/foo/foo.go", "package foo\n");
        write(
            dir.path(),
            "internal/foo/foo_test.go",
            "package foo\nfunc TestFoo() {}\n",
        );
        let (candidates, _) =
            discover_candidate_tests(dir.path(), &["internal/foo/foo.go".to_string()], &[], true);
        assert!(candidates.iter().any(
            |c| c.path == "internal/foo/foo_test.go" && c.confidence == TestConfidence::Likely
        ));
    }

    #[test]
    fn go_test_file_itself_is_exact() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "pkg/a_test.go", "package p\nfunc TestA() {}\n");
        let (candidates, _) =
            discover_candidate_tests(dir.path(), &["pkg/a_test.go".to_string()], &[], true);
        assert_eq!(candidates[0].confidence, TestConfidence::Exact);
    }

    #[test]
    fn typescript_test_and_spec_are_exact() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "src/a.test.ts", "test('a', () => {})\n");
        write(dir.path(), "src/b.spec.ts", "describe('b', () => {})\n");
        let (candidates, _) = discover_candidate_tests(
            dir.path(),
            &["src/a.test.ts".to_string(), "src/b.spec.ts".to_string()],
            &[],
            true,
        );
        assert!(
            candidates
                .iter()
                .all(|c| c.confidence == TestConfidence::Exact)
        );
    }

    #[test]
    fn typescript_sibling_is_likely_and_missing_script_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "src/foo.ts", "export const x = 1;\n");
        write(dir.path(), "src/foo.test.ts", "test('x', () => {})\n");
        write(dir.path(), "package.json", "{\"scripts\": {}}\n");
        let (candidates, warnings) =
            discover_candidate_tests(dir.path(), &["src/foo.ts".to_string()], &[], true);
        assert!(
            candidates
                .iter()
                .any(|c| c.path == "src/foo.test.ts" && c.confidence == TestConfidence::Likely)
        );
        assert!(warnings.iter().any(|w| w.contains("no test script")));
    }

    #[test]
    fn python_test_prefix_is_exact_and_sibling_is_likely() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "tests/test_foo.py", "def test_foo(): pass\n");
        write(dir.path(), "src/foo.py", "x = 1\n");
        write(dir.path(), "src/test_foo.py", "def test_foo(): pass\n");
        let (exact, _) =
            discover_candidate_tests(dir.path(), &["tests/test_foo.py".to_string()], &[], true);
        assert_eq!(exact[0].confidence, TestConfidence::Exact);
        let (likely, _) =
            discover_candidate_tests(dir.path(), &["src/foo.py".to_string()], &[], true);
        assert!(
            likely
                .iter()
                .any(|c| c.path == "src/test_foo.py" && c.confidence == TestConfidence::Likely)
        );
    }

    #[test]
    fn venv_layout_does_not_change_discovery() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), ".venv/pyvenv.cfg", "home = x\n");
        write(dir.path(), "src/foo.py", "x = 1\n");
        let (candidates, _) =
            discover_candidate_tests(dir.path(), &["src/foo.py".to_string()], &[], true);
        // No sibling: empty with a coverage warning, not invented confidence.
        assert!(candidates.is_empty());
    }
}
