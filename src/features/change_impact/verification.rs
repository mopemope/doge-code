use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::types::{CandidateTest, TestConfidence, VerificationRecommendation};

/// Languages relevant to the change set (from file extensions).
fn languages_in_scope(changed: &[String], impacted: &[String]) -> Vec<String> {
    let mut langs: BTreeSet<String> = BTreeSet::new();
    for p in changed.iter().chain(impacted.iter()) {
        let lower = p.to_ascii_lowercase();
        if lower.ends_with(".rs") {
            langs.insert("rust".to_string());
        } else if lower.ends_with(".go") {
            langs.insert("go".to_string());
        } else if lower.ends_with(".ts")
            || lower.ends_with(".tsx")
            || lower.ends_with(".js")
            || lower.ends_with(".jsx")
            || lower.ends_with(".mjs")
            || lower.ends_with(".cjs")
        {
            langs.insert("typescript".to_string());
        } else if lower.ends_with(".py") {
            langs.insert("python".to_string());
        }
    }
    langs.into_iter().collect()
}

/// Project languages as a fallback when the change set carries no
/// recognizable extension (deleted files, unknown kinds).
fn fallback_project_languages(project_root: &Path) -> Vec<String> {
    crate::features::testing::detect_project_languages(project_root)
}

/// TypeScript test commands from the existing project configuration.
/// Reuses the same `test` / `test:unit` / `test:all` preference as the
/// trusted `/test` path; unknown runners yield no command (never invented).
fn typescript_commands(project_root: &Path) -> Vec<(String, Vec<String>)> {
    let path = project_root.join("package.json");
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return vec![];
    };
    let Some(scripts) = value.get("scripts").and_then(|v| v.as_object()) else {
        return vec![];
    };
    for name in ["test", "test:unit", "test:all"] {
        if scripts.contains_key(name) {
            return vec![("npm".to_string(), vec!["run".to_string(), name.to_string()])];
        }
    }
    vec![]
}

/// Python test command preserving the existing venv selection behavior.
/// Never silently switches interpreters: `.venv` wins, then `venv`,
/// then the system interpreter.
fn python_command(project_root: &Path) -> (String, Vec<String>) {
    let interpreter = [".venv", "venv"].into_iter().find_map(|dir| {
        let env = project_root.join(dir);
        env.join("pyvenv.cfg").is_file().then(|| {
            env.join(if cfg!(windows) {
                "Scripts/python.exe"
            } else {
                "bin/python"
            })
            .to_string_lossy()
            .into_owned()
        })
    });
    let program = interpreter.unwrap_or_else(|| {
        if cfg!(windows) {
            "python".to_string()
        } else {
            "python3".to_string()
        }
    });
    (
        program,
        vec!["-m".to_string(), "pytest".to_string(), "-v".to_string()],
    )
}

/// Package directory for a Go file (e.g. `internal/foo/foo.go` ->
/// `internal/foo`). Empty for root-level files.
fn go_package_dir(path: &str) -> String {
    path.rfind('/')
        .map(|i| path[..i].to_string())
        .unwrap_or_default()
}

/// Whether every Go file in scope shares one package directory that
/// contains at least one `*_test.go` file.
fn go_single_tested_package(
    project_root: &Path,
    changed: &[String],
    impacted: &[String],
) -> Option<String> {
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for p in changed.iter().chain(impacted.iter()) {
        if p.to_ascii_lowercase().ends_with(".go") {
            dirs.insert(go_package_dir(p));
        }
    }
    if dirs.len() != 1 {
        return None;
    }
    let dir = dirs.into_iter().next().unwrap_or_default();
    let abs = if dir.is_empty() {
        project_root.to_path_buf()
    } else {
        project_root.join(&dir)
    };
    let Ok(entries) = std::fs::read_dir(abs) else {
        return None;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with("_test.go") && entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            return Some(dir);
        }
    }
    None
}

/// Build verification recommendations (no execution, no obligation writes).
///
/// Rules:
/// - Exact candidates -> focused candidates where invocation semantics are
///   established (Go package, Python/Rust project commands); Rust focused
///   filters are never guessed, so Rust stays project-level.
/// - Package-level where the affected package is reliably known (Go).
/// - Otherwise project-level broad fallback.
/// - Truncated graphs, unknown coverage, and missing test configuration
///   always recommend broad verification.
/// - A focused pass never proves the repository correct: every focused
///   recommendation carries an explicit coverage disclaimer.
/// - Obligation matches are reported as "may satisfy ... if executed",
///   never as fulfilled. Final matching stays with the provenance subsystem.
#[allow(clippy::too_many_arguments)]
pub fn plan_verification(
    project_root: &Path,
    changed: &[String],
    impacted: &[String],
    candidates: &[CandidateTest],
    graph_truncated: bool,
    analysis_partial: bool,
    plan_items: &[crate::tools::plan::PlanItem],
) -> (Vec<VerificationRecommendation>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut recs: Vec<VerificationRecommendation> = Vec::new();

    let mut langs = languages_in_scope(changed, impacted);
    if langs.is_empty() {
        langs = fallback_project_languages(project_root);
        if !langs.is_empty() {
            warnings.push(
                "Changed files carry no recognizable language; project-level verification was inferred from project configuration."
                    .to_string(),
            );
        }
    }
    if langs.is_empty() {
        warnings.push(
            "No test configuration was detected for the changed files; no verification command could be recommended."
                .to_string(),
        );
        return (recs, warnings);
    }

    let has_exact = candidates
        .iter()
        .any(|c| c.confidence == TestConfidence::Exact);
    if !has_exact {
        warnings.push(
            "No exact test coverage was established; static candidates are not runtime coverage evidence. Broader verification is recommended."
                .to_string(),
        );
    }
    if graph_truncated {
        warnings.push(
            "Dependency traversal was truncated; focused tests cannot claim complete coverage. Broader verification is recommended."
                .to_string(),
        );
    }
    if analysis_partial {
        warnings.push(
            "Analysis is partial (missing or stale RepoMap entries); broader verification is recommended."
                .to_string(),
        );
    }

    // Current obligations for attribution hints (read-only).
    let current = crate::provenance::obligations::current_obligations(plan_items);
    let obligation_hint = |program: &str,
                           args: &[String],
                           kind: crate::provenance::VerificationKind|
     -> String {
        let mut matched: Vec<String> = Vec::new();
        for ob in &current {
            if crate::provenance::obligations::obligation_matches_invocation(
                &ob.obligation,
                kind,
                program,
                args,
            ) {
                matched.push(ob.obligation.id.clone());
            }
        }
        matched.sort();
        matched.dedup();
        if matched.is_empty() {
            String::new()
        } else {
            format!(
                " May satisfy obligation(s) {} if executed and matched by the provenance subsystem.",
                matched.join(", ")
            )
        }
    };

    for lang in &langs {
        match lang.as_str() {
            "rust" => {
                let program = "cargo".to_string();
                let args = vec!["test".to_string()];
                let hint =
                    obligation_hint(&program, &args, crate::provenance::VerificationKind::Test);
                recs.push(VerificationRecommendation {
                    kind: "test".to_string(),
                    program,
                    args,
                    cwd: None,
                    reason: format!(
                        "Project-level Rust verification (cargo test). A passing focused test does not prove the whole repository correct; this broad fallback covers unit and integration tests.{hint}"
                    ),
                    confidence: "high".to_string(),
                    coverage_scope: "project".to_string(),
                });
            }
            "go" => {
                if let Some(pkg) = go_single_tested_package(project_root, changed, impacted) {
                    let args = if pkg.is_empty() {
                        vec!["test".to_string(), "-json".to_string(), ".".to_string()]
                    } else {
                        vec!["test".to_string(), "-json".to_string(), format!("./{pkg}")]
                    };
                    let hint =
                        obligation_hint("go", &args, crate::provenance::VerificationKind::Test);
                    recs.push(VerificationRecommendation {
                        kind: "test".to_string(),
                        program: "go".to_string(),
                        args,
                        cwd: None,
                        reason: format!(
                            "Package-level Go verification for './{}' (reliable package association). Does not prove whole-repository correctness; the project-level fallback below covers transitive callers.{hint}",
                            if pkg.is_empty() { "." } else { &pkg }
                        ),
                        confidence: "medium".to_string(),
                        coverage_scope: "package".to_string(),
                    });
                    // Broad fallback always accompanies a focused/package pick.
                    let broad = vec!["test".to_string(), "-json".to_string(), "./...".to_string()];
                    let broad_hint =
                        obligation_hint("go", &broad, crate::provenance::VerificationKind::Test);
                    recs.push(VerificationRecommendation {
                        kind: "test".to_string(),
                        program: "go".to_string(),
                        args: broad,
                        cwd: None,
                        reason: format!(
                            "Project-level Go verification fallback (go test -json ./...).{broad_hint}"
                        ),
                        confidence: "high".to_string(),
                        coverage_scope: "project".to_string(),
                    });
                } else {
                    let args = vec!["test".to_string(), "-json".to_string(), "./...".to_string()];
                    let hint =
                        obligation_hint("go", &args, crate::provenance::VerificationKind::Test);
                    // Multi-package or unknown package: project-level only.
                    let scope_note = if changed
                        .iter()
                        .chain(impacted.iter())
                        .filter(|p| p.to_ascii_lowercase().ends_with(".go"))
                        .count()
                        > 1
                    {
                        " Multiple Go packages are affected; a single package command cannot cover all transitive callers."
                    } else {
                        " Package-level coverage could not be established; a test in one package does not cover all transitive callers."
                    };
                    recs.push(VerificationRecommendation {
                        kind: "test".to_string(),
                        program: "go".to_string(),
                        args,
                        cwd: None,
                        reason: format!(
                            "Project-level Go verification (go test -json ./...).{scope_note}{hint}"
                        ),
                        confidence: "high".to_string(),
                        coverage_scope: "project".to_string(),
                    });
                }
            }
            "typescript" => {
                let cmds = typescript_commands(project_root);
                if cmds.is_empty() {
                    warnings.push(
                        "TypeScript files changed but no test script (test/test:unit/test:all) was found in package.json; no TypeScript verification command could be recommended."
                            .to_string(),
                    );
                } else {
                    for (program, args) in cmds {
                        let hint = obligation_hint(
                            &program,
                            &args,
                            crate::provenance::VerificationKind::Test,
                        );
                        recs.push(VerificationRecommendation {
                            kind: "test".to_string(),
                            program,
                            args,
                            cwd: None,
                            reason: format!(
                                "Project-configured TypeScript verification (package.json test script). Runner-specific focused invocations were not invented; use the configured command.{hint}"
                            ),
                            confidence: "high".to_string(),
                            coverage_scope: "project".to_string(),
                        });
                    }
                }
            }
            "python" => {
                let (program, args) = python_command(project_root);
                let hint =
                    obligation_hint(&program, &args, crate::provenance::VerificationKind::Test);
                recs.push(VerificationRecommendation {
                    kind: "test".to_string(),
                    program,
                    args,
                    cwd: None,
                    reason: format!(
                        "Project-configured Python verification (pytest, preserving virtual environment selection). Does not switch interpreters.{hint}"
                    ),
                    confidence: "high".to_string(),
                    coverage_scope: "project".to_string(),
                });
            }
            _ => {}
        }
    }

    // Deterministic order; dedup identical commands.
    recs.sort_by(|a, b| {
        a.program
            .cmp(&b.program)
            .then(a.args.cmp(&b.args))
            .then(a.coverage_scope.cmp(&b.coverage_scope))
    });
    let mut deduped: Vec<VerificationRecommendation> = Vec::new();
    let mut seen_cmds: BTreeMap<(String, Vec<String>), bool> = BTreeMap::new();
    for r in recs {
        let key = (r.program.clone(), r.args.clone());
        if seen_cmds.insert(key, true).is_none() {
            deduped.push(r);
        }
    }
    (deduped, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::plan::PlanItem;

    fn write(root: &Path, rel: &str, content: &str) {
        let abs = root.join(rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).expect("mkdir");
        }
        std::fs::write(abs, content).expect("write");
    }

    fn candidate(path: &str, confidence: TestConfidence, language: &str) -> CandidateTest {
        CandidateTest {
            path: path.to_string(),
            confidence,
            language: language.to_string(),
        }
    }

    #[test]
    fn exact_candidates_still_include_broad_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "Cargo.toml", "[package]\n");
        write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        let (recs, _) = plan_verification(
            dir.path(),
            &["src/lib.rs".to_string()],
            &[],
            &[candidate("tests/api.rs", TestConfidence::Exact, "rust")],
            false,
            false,
            &[],
        );
        assert!(
            recs.iter()
                .any(|r| r.program == "cargo" && r.coverage_scope == "project")
        );
        assert!(recs.iter().all(|r| r.reason.contains("does not prove")
            || r.reason.contains("Broad")
            || r.reason.contains("fallback")
            || r.reason.contains("Project-level")));
    }

    #[test]
    fn go_single_package_recommends_package_plus_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "go.mod", "module example\n");
        write(dir.path(), "internal/foo/foo.go", "package foo\n");
        write(
            dir.path(),
            "internal/foo/foo_test.go",
            "package foo\nfunc TestFoo() {}\n",
        );
        let (recs, _) = plan_verification(
            dir.path(),
            &["internal/foo/foo.go".to_string()],
            &[],
            &[candidate(
                "internal/foo/foo_test.go",
                TestConfidence::Likely,
                "go",
            )],
            false,
            false,
            &[],
        );
        assert!(
            recs.iter().any(|r| r.coverage_scope == "package"
                && r.args.iter().any(|a| a.contains("internal/foo")))
        );
        assert!(
            recs.iter()
                .any(|r| r.coverage_scope == "project" && r.args.contains(&"./...".to_string()))
        );
    }

    #[test]
    fn go_multi_package_falls_back_to_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "go.mod", "module example\n");
        write(dir.path(), "a/a.go", "package a\n");
        write(dir.path(), "b/b.go", "package b\n");
        let (recs, _) = plan_verification(
            dir.path(),
            &["a/a.go".to_string(), "b/b.go".to_string()],
            &[],
            &[],
            false,
            false,
            &[],
        );
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].coverage_scope, "project");
    }

    #[test]
    fn truncated_graph_recommends_broad() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "Cargo.toml", "[package]\n");
        let (recs, warnings) = plan_verification(
            dir.path(),
            &["src/lib.rs".to_string()],
            &[],
            &[],
            true,
            false,
            &[],
        );
        assert!(recs.iter().any(|r| r.coverage_scope == "project"));
        assert!(warnings.iter().any(|w| w.contains("truncated")));
    }

    #[test]
    fn multi_language_produces_each_ecosystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "Cargo.toml", "[package]\n");
        write(
            dir.path(),
            "package.json",
            "{\"scripts\": {\"test\": \"x\"}}\n",
        );
        let (recs, _) = plan_verification(
            dir.path(),
            &["src/lib.rs".to_string(), "src/app.ts".to_string()],
            &[],
            &[],
            false,
            false,
            &[],
        );
        assert!(recs.iter().any(|r| r.program == "cargo"));
        assert!(recs.iter().any(|r| r.program == "npm"));
    }

    #[test]
    fn unknown_coverage_warns_and_falls_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "Cargo.toml", "[package]\n");
        let (recs, warnings) = plan_verification(
            dir.path(),
            &["src/lib.rs".to_string()],
            &[],
            &[],
            false,
            false,
            &[],
        );
        assert!(!recs.is_empty());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("not runtime coverage") || w.contains("No exact test"))
        );
    }

    #[test]
    fn no_test_configuration_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (recs, warnings) = plan_verification(
            dir.path(),
            &["notes.txt".to_string()],
            &[],
            &[],
            false,
            false,
            &[],
        );
        assert!(recs.is_empty());
        assert!(warnings.iter().any(|w| w.contains("No test configuration")));
    }

    #[test]
    fn obligation_match_is_hint_not_fulfillment() {
        use crate::provenance::types::{
            VerificationCommandMatcher, VerificationKind, VerificationObligation,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "Cargo.toml", "[package]\n");
        let plan = vec![PlanItem {
            id: "step-1".to_string(),
            parent_id: None,
            content: "work".to_string(),
            status: "in_progress".to_string(),
            requirement_ids: vec![],
            verification_obligations: vec![VerificationObligation {
                id: "vo-1".to_string(),
                description: "run cargo test".to_string(),
                kind: VerificationKind::Test,
                command: Some(VerificationCommandMatcher {
                    program: "cargo".to_string(),
                    args_prefix: vec!["test".to_string()],
                }),
            }],
        }];
        let (recs, _) = plan_verification(
            dir.path(),
            &["src/lib.rs".to_string()],
            &[],
            &[candidate("src/lib.rs", TestConfidence::Exact, "rust")],
            false,
            false,
            &plan,
        );
        assert!(recs[0].reason.contains("May satisfy"));
        assert!(!recs[0].reason.contains("fulfilled"));
        // Recommendations never mark obligations: no provenance write happens here.
        assert!(plan[0].verification_obligations[0].id == "vo-1");
    }

    #[test]
    fn typescript_without_script_warns() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "package.json", "{\"scripts\": {}}\n");
        let (recs, warnings) = plan_verification(
            dir.path(),
            &["src/app.ts".to_string()],
            &[],
            &[],
            false,
            false,
            &[],
        );
        assert!(recs.is_empty());
        assert!(warnings.iter().any(|w| w.contains("no test script")));
    }

    #[test]
    fn python_preserves_venv_interpreter() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".venv/bin")).expect("mkdir");
        std::fs::write(dir.path().join(".venv/pyvenv.cfg"), "home = x\n").expect("write");
        let (recs, _) = plan_verification(
            dir.path(),
            &["src/foo.py".to_string()],
            &[],
            &[],
            false,
            false,
            &[],
        );
        assert_eq!(recs.len(), 1);
        assert!(recs[0].program.contains(".venv"));
        assert_eq!(recs[0].args, vec!["-m", "pytest", "-v"]);
    }
}
