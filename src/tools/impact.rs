//! `impact_analyze`: read-only change impact + verification planning.
//!
//! Identifies which files/symbols may be affected by a code change and
//! recommends the smallest reasonably sufficient verification commands.
//! Never modifies source, never executes tests, never fulfills
//! verification obligations, and never rewrites provenance history.

use crate::features::change_impact::{
    DEFAULT_RESPONSE_BUDGET_CHARS, MAX_RESPONSE_BUDGET_CHARS, analyze_from_collected,
    collect_active_changes, collect_explicit_changes,
};
use crate::llm::types::{ToolDef, ToolFunctionDef};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const IMPACT_ANALYZE_TOOL_NAME: &str = "impact_analyze";

pub fn tool_def() -> ToolDef {
    ToolDef {
        kind: "function".to_string(),
        function: ToolFunctionDef {
            name: IMPACT_ANALYZE_TOOL_NAME.to_string(),
            description: "Analyze which files and callers may be affected by changed files and recommend verification commands. Read-only: never edits code, never runs tests, never marks verification complete. Use after meaningful code changes when targeted verification would help; prefer the recommended broad fallback when coverage is uncertain.".to_string(),
            strict: None,
            parameters: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Project-relative changed files. When omitted, the current session's active tracked changes are analyzed."
                    },
                    "max_depth": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 5,
                        "description": "Reverse dependency traversal depth (default 2)."
                    },
                    "include_tests": {
                        "type": "boolean",
                        "description": "Discover candidate test files (default true)."
                    },
                    "response_budget_chars": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 8000,
                        "description": "Total response budget in chars (default 6000, max 8000)."
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
        },
    }
}

/// Arguments for `impact_analyze`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactAnalyzeArgs {
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    #[serde(default)]
    pub max_depth: Option<u8>,
    #[serde(default)]
    pub include_tests: Option<bool>,
    #[serde(default)]
    pub response_budget_chars: Option<usize>,
}

/// Execute `impact_analyze` against the current session (read-only).
///
/// - Explicit `paths` win; otherwise the session's active tracked changes.
/// - No filesystem mutation, no provenance mutation, no test execution.
/// - Cancellation is checked at safe boundaries before heavy traversal.
pub async fn impact_analyze(
    fs_tools: &crate::tools::FsTools,
    args: ImpactAnalyzeArgs,
    cancel: Option<tokio_util::sync::CancellationToken>,
) -> anyhow::Result<crate::features::change_impact::ImpactAnalysisResponse> {
    if cancel
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
    }
    let max_depth = args
        .max_depth
        .unwrap_or(crate::features::change_impact::graph::DEFAULT_MAX_DEPTH);
    if max_depth > crate::features::change_impact::graph::MAX_DEPTH_LIMIT {
        anyhow::bail!(
            "invalid argument: max_depth must be between 0 and {}",
            crate::features::change_impact::graph::MAX_DEPTH_LIMIT
        );
    }
    let include_tests = args.include_tests.unwrap_or(true);
    let budget = args
        .response_budget_chars
        .unwrap_or(DEFAULT_RESPONSE_BUDGET_CHARS)
        .clamp(1, MAX_RESPONSE_BUDGET_CHARS);
    let project_root = &fs_tools.config.project_root;

    // Read-only RepoMap snapshot: never rebuild persistent analysis state.
    let repomap = fs_tools.current_repomap_snapshot().await;
    let identity = repomap
        .as_ref()
        .and_then(|map| crate::analysis::SymbolIdentityIndex::build(map, project_root).ok());
    if repomap.is_none() {
        tracing::info!("impact_analyze: repomap unavailable; partial analysis");
    }

    // Change collection.
    let mut early_warnings: Vec<String> = Vec::new();
    let (changed_files, mut warnings) = if let Some(paths) = args.paths {
        if paths.is_empty() {
            (vec![], vec![])
        } else {
            let collected = collect_explicit_changes(project_root, &paths, repomap.as_ref());
            (collected.files, collected.warnings)
        }
    } else {
        // Session active tracked changes (read-only provenance query).
        let (events, session_files) = match crate::tools::provenance::load_current_events(fs_tools)
        {
            Ok(Some(loaded)) => {
                if !loaded.warnings.is_empty() {
                    early_warnings.push(
                        "Provenance load reported warnings; analysis may be partial.".to_string(),
                    );
                }
                let session = fs_tools.get_current_session();
                let files = session.map(|s| s.changed_files).unwrap_or_default();
                (loaded.events, files)
            }
            Ok(None) => (vec![], vec![]),
            Err(e) => {
                tracing::debug!(error = %e, "impact_analyze: provenance load failed");
                early_warnings.push(format!(
                    "Provenance events could not be loaded ({e}); falling back to session changed files with unknown confidence."
                ));
                (
                    vec![],
                    fs_tools
                        .get_current_session()
                        .map(|s| s.changed_files)
                        .unwrap_or_default(),
                )
            }
        };
        if cancel
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
        }
        let collected =
            collect_active_changes(project_root, &events, &session_files, repomap.as_ref());
        let mut w = early_warnings;
        w.extend(collected.warnings);
        (collected.files, w)
    };

    // Empty explicit list and empty active set both mean "no changes":
    // return a valid no-changes result instead of guessing a file set.
    // (Explicit empty `paths: []` is treated the same as no active changes.)
    let plan_items = match fs_tools.plan_read() {
        Ok(plan) => plan.items,
        Err(e) => {
            tracing::debug!(error = %e, "impact_analyze: plan read failed");
            warnings.push(
                "Plan obligations could not be read; verification hints omit obligation attribution."
                    .to_string(),
            );
            vec![]
        }
    };

    if cancel
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(anyhow::anyhow!(crate::llm::LlmErrorKind::Cancelled));
    }
    tracing::info!(
        changed = changed_files.len(),
        max_depth,
        include_tests,
        "impact_analyze completed"
    );
    Ok(analyze_from_collected(
        project_root,
        repomap.as_ref(),
        identity.as_ref(),
        changed_files,
        warnings,
        max_depth,
        include_tests,
        budget,
        &plan_items,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::tools::FsTools;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn test_fs(project_root: std::path::PathBuf) -> FsTools {
        let config = Arc::new(AppConfig {
            project_root,
            ..Default::default()
        });
        FsTools::new(Arc::new(RwLock::new(None)), config)
    }

    fn write(root: &std::path::Path, rel: &str, content: &str) {
        let abs = root.join(rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).expect("mkdir");
        }
        std::fs::write(abs, content).expect("write");
    }

    #[tokio::test]
    async fn valid_explicit_input_returns_bounded_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "src/lib.rs", "pub fn foo() {}\n");
        write(dir.path(), "Cargo.toml", "[package]\n");
        let fs = test_fs(dir.path().to_path_buf());
        let res = impact_analyze(
            &fs,
            ImpactAnalyzeArgs {
                paths: Some(vec!["src/lib.rs".to_string()]),
                max_depth: Some(2),
                include_tests: Some(true),
                response_budget_chars: Some(6000),
            },
            None,
        )
        .await
        .expect("analyze");
        assert!(res.ok);
        assert_eq!(res.changed_files, vec!["src/lib.rs"]);
        let serialized = serde_json::to_string(&res).expect("json");
        assert!(serialized.chars().count() <= 6000);
        // No filesystem mutation.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/lib.rs")).expect("read"),
            "pub fn foo() {}\n"
        );
        // No provenance mutation: no session manager, so no events to check;
        // the call must simply succeed without writing.
    }

    #[tokio::test]
    async fn invalid_max_depth_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path().to_path_buf());
        let err = impact_analyze(
            &fs,
            ImpactAnalyzeArgs {
                paths: Some(vec!["a.rs".to_string()]),
                max_depth: Some(9),
                include_tests: None,
                response_budget_chars: None,
            },
            None,
        )
        .await
        .expect_err("depth");
        assert!(err.to_string().contains("max_depth"));
    }

    #[tokio::test]
    async fn empty_result_when_no_active_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path().to_path_buf());
        let res = impact_analyze(
            &fs,
            ImpactAnalyzeArgs {
                paths: None,
                max_depth: None,
                include_tests: None,
                response_budget_chars: None,
            },
            None,
        )
        .await
        .expect("analyze");
        assert!(res.ok);
        assert_eq!(
            res.analysis_status,
            crate::features::change_impact::AnalysisStatus::NoChanges
        );
        assert!(res.changed_files.is_empty());
    }

    #[tokio::test]
    async fn large_result_is_budgeted() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..30 {
            write(dir.path(), &format!("src/f{i}.rs"), "pub fn f() {}\n");
        }
        write(dir.path(), "Cargo.toml", "[package]\n");
        let fs = test_fs(dir.path().to_path_buf());
        let paths: Vec<String> = (0..30).map(|i| format!("src/f{i}.rs")).collect();
        let res = impact_analyze(
            &fs,
            ImpactAnalyzeArgs {
                paths: Some(paths),
                max_depth: Some(2),
                include_tests: Some(true),
                response_budget_chars: Some(1500),
            },
            None,
        )
        .await
        .expect("analyze");
        let serialized = serde_json::to_string(&res).expect("json");
        // The tool owns its budget: a tight budget must either fit or be
        // reported as truncated, while verification is never dropped.
        assert!(
            serialized.chars().count() <= 1500 || res.truncated == Some(true),
            "budget exceeded without truncation report ({} chars)",
            serialized.chars().count()
        );
        assert!(!res.verification.is_empty());
    }

    #[test]
    fn schema_is_small_and_stable() {
        let def = tool_def();
        assert_eq!(def.function.name, "impact_analyze");
        let rendered = serde_json::to_string(&def).expect("serialize");
        assert!(
            rendered.len() < 2_500,
            "impact_analyze schema grew: {} bytes",
            rendered.len()
        );
        let params = &def.function.parameters;
        assert!(params.get("properties").is_some());
        assert_eq!(params["additionalProperties"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn no_provenance_mutation_on_read_only_call() {
        use crate::session::SessionManager;
        use crate::session::SessionStore;
        let dir = tempfile::tempdir().expect("tempdir");
        let project_root = dir.path().to_path_buf();
        write(&project_root, "a.txt", "hello\n");
        let store = SessionStore::new(project_root.join(".doge/sessions")).expect("store");
        let manager = Arc::new(std::sync::Mutex::new(SessionManager {
            save_state: Default::default(),
            current_lease: None,
            store,
            current_session: None,
        }));
        manager
            .lock()
            .unwrap()
            .create_session(None)
            .expect("session");
        let session_id = manager.lock().unwrap().current_session_id().expect("id");
        let session_dir = project_root.join(".doge/sessions").join(&session_id);
        let config = Arc::new(AppConfig {
            project_root: project_root.clone(),
            ..Default::default()
        });
        let fs = FsTools::new(Arc::new(RwLock::new(None)), config).with_session_manager(manager);
        let before = crate::provenance::ProvenanceStore::new(session_dir.clone())
            .load_all()
            .map(|l| l.events.len())
            .unwrap_or(0);
        let res = impact_analyze(
            &fs,
            ImpactAnalyzeArgs {
                paths: Some(vec!["a.txt".to_string()]),
                max_depth: Some(1),
                include_tests: Some(false),
                response_budget_chars: Some(6000),
            },
            None,
        )
        .await
        .expect("analyze");
        assert!(res.ok);
        let after = crate::provenance::ProvenanceStore::new(session_dir)
            .load_all()
            .map(|l| l.events.len())
            .unwrap_or(0);
        assert_eq!(before, after, "read-only call must not append provenance");
    }

    #[tokio::test]
    async fn cancelled_call_returns_typed_cancellation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path().to_path_buf());
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let err = impact_analyze(
            &fs,
            ImpactAnalyzeArgs {
                paths: Some(vec!["a.rs".to_string()]),
                max_depth: None,
                include_tests: None,
                response_budget_chars: None,
            },
            Some(token),
        )
        .await
        .expect_err("cancelled");
        assert!(err.downcast_ref::<crate::llm::LlmErrorKind>().is_some());
    }

    #[test]
    fn deferred_routing_preserves_core_eager_surface() {
        // New tool must be discoverable via deferred routing without joining
        // the always-visible core set (no mandatory extra iteration).
        let names: Vec<String> = crate::llm::default_tools_def()
            .iter()
            .map(|def| def.function.name.clone())
            .collect();
        assert!(names.contains(&"impact_analyze".to_string()));
        assert!(!crate::llm::CORE_EAGER_TOOLS.contains(&"impact_analyze"));
    }

    #[test]
    fn response_schema_is_stable_and_project_relative() {
        use crate::features::change_impact::{AnalysisStatus, ImpactAnalysisResponse, ImpactGraph};
        let res = ImpactAnalysisResponse {
            ok: true,
            analysis_status: AnalysisStatus::Complete,
            changed_files: vec!["src/price.rs".to_string()],
            impacted_files: vec!["src/order.rs".to_string()],
            graph: ImpactGraph {
                nodes: vec![],
                edges: vec![],
                truncated: false,
            },
            candidate_tests: vec![],
            verification: vec![crate::features::change_impact::VerificationRecommendation {
                kind: "test".to_string(),
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
                cwd: None,
                reason: "Broader fallback verification".to_string(),
                confidence: "high".to_string(),
                coverage_scope: "project".to_string(),
            }],
            warnings: vec![],
            truncated: None,
        };
        let value = serde_json::to_value(&res).expect("json");
        assert_eq!(value["ok"], true);
        assert_eq!(value["analysis_status"], "complete");
        assert_eq!(value["changed_files"][0], "src/price.rs");
        assert_eq!(value["verification"][0]["program"], "cargo");
        // Structured program + args, never an opaque shell string.
        assert!(value["verification"][0].get("args").is_some());
        assert!(value["verification"][0].get("command").is_none());
        // No absolute paths leak.
        let serialized = serde_json::to_string(&value).expect("serialize");
        assert!(!serialized.contains("/tmp/"));
        assert!(!serialized.contains("/home/"));
    }

    #[tokio::test]
    async fn invalid_input_is_actionable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = test_fs(dir.path().to_path_buf());
        // Unknown field must fail before any analysis.
        let bad = serde_json::json!({
            "paths": ["a.rs"],
            "unknown_field": true
        });
        let err = serde_json::from_value::<ImpactAnalyzeArgs>(bad).expect_err("unknown");
        assert!(err.to_string().contains("unknown field"));
        let _ = fs;
    }

    #[tokio::test]
    async fn dispatch_routes_impact_analyze() {
        use crate::llm::tool_runtime::ToolRuntime;
        use crate::llm::types::{ToolCall, ToolCallFunction};
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "a.txt", "hello\n");
        let config = Arc::new(AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        });
        let fs = FsTools::new(Arc::new(RwLock::new(None)), config);
        let runtime = ToolRuntime::build(&fs, None, "test-model", None)
            .await
            .expect("runtime");
        runtime
            .tool_catalog
            .activate(&["impact_analyze".to_string()])
            .await;
        let call = ToolCall {
            id: Some("impact-1".into()),
            r#type: "function".into(),
            function: ToolCallFunction {
                name: "impact_analyze".into(),
                arguments: serde_json::json!({"paths": ["a.txt"]}).to_string(),
            },
        };
        let output = crate::llm::tool_execution::dispatch_tool_call(&runtime, &call)
            .await
            .expect("dispatch");
        assert!(output.is_success);
        assert_eq!(output.value["ok"], true);
        assert!(output.value.get("changed_files").is_some());
        assert!(output.value.get("verification").is_some());
    }
}
