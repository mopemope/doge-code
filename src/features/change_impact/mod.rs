pub mod changes;
pub mod discovery;
pub mod graph;
pub mod types;
pub mod verification;

pub use changes::{collect_active_changes, collect_explicit_changes, identify_changed_symbols};
pub use discovery::discover_candidate_tests;
pub use graph::{
    MAX_DEPTH_LIMIT, MAX_EDGES, MAX_NODES, build_impact_graph, impacted_files_from_graph,
};
pub use types::{
    AnalysisStatus, CandidateTest, ChangeKind, ChangedFile, ChangedSymbol, EdgeConfidence,
    IdentificationConfidence, ImpactAnalysisResponse, ImpactEdge, ImpactGraph, ImpactNode,
    TestConfidence, VerificationRecommendation,
};
pub use verification::plan_verification;

use std::path::Path;

/// Default bounded response budget (chars of serialized JSON).
pub const DEFAULT_RESPONSE_BUDGET_CHARS: usize = 6_000;
/// Hard cap: the global default tier is 8,000 chars.
pub const MAX_RESPONSE_BUDGET_CHARS: usize = 8_000;

/// Assemble changed + impacted file lists, graph, candidates, and
/// verification into one deterministic response.
#[allow(clippy::too_many_arguments)]
pub fn analyze_from_collected(
    project_root: &Path,
    repomap: Option<&crate::analysis::RepoMap>,
    identity: Option<&crate::analysis::SymbolIdentityIndex>,
    changed_files: Vec<ChangedFile>,
    mut change_warnings: Vec<String>,
    max_depth: u8,
    include_tests: bool,
    response_budget_chars: usize,
    plan_items: &[crate::tools::plan::PlanItem],
) -> ImpactAnalysisResponse {
    let budget = response_budget_chars.clamp(1, MAX_RESPONSE_BUDGET_CHARS);
    if changed_files.is_empty() {
        let mut response = ImpactAnalysisResponse {
            ok: true,
            analysis_status: AnalysisStatus::NoChanges,
            changed_files: vec![],
            impacted_files: vec![],
            graph: ImpactGraph {
                nodes: vec![],
                edges: vec![],
                truncated: false,
            },
            candidate_tests: vec![],
            verification: vec![],
            warnings: vec![
                "No active changes were found; no verification is recommended. Supply explicit paths to analyze untracked edits.".to_string(),
            ],
            truncated: None,
        };
        response.warnings.extend(change_warnings);
        return apply_budget(response, budget);
    }

    let repomap_missing = repomap.is_none();
    let (changed_symbols, mut symbol_warnings) =
        identify_changed_symbols(project_root, repomap, identity, &changed_files);
    change_warnings.append(&mut symbol_warnings);

    let (graph, mut graph_warnings) = match repomap {
        Some(map) => build_impact_graph(
            project_root,
            map,
            identity,
            &changed_symbols,
            max_depth,
            MAX_NODES,
            MAX_EDGES,
        ),
        None => (
            ImpactGraph {
                nodes: vec![],
                edges: vec![],
                truncated: false,
            },
            vec![],
        ),
    };
    change_warnings.append(&mut graph_warnings);

    let changed_paths: Vec<String> = changed_files.iter().map(|f| f.path.clone()).collect();
    let impacted_files: Vec<String> = impacted_files_from_graph(&graph, &changed_paths);

    let (candidates, mut discovery_warnings) =
        discover_candidate_tests(project_root, &changed_paths, &impacted_files, include_tests);
    change_warnings.append(&mut discovery_warnings);

    let analysis_partial = repomap_missing
        || changed_symbols
            .iter()
            .any(|s| s.identification_confidence == IdentificationConfidence::Unresolved)
        || change_warnings
            .iter()
            .any(|w| w.contains("RepoMap") || w.contains("unknown") || w.contains("Unknown"));
    let (verification, mut verify_warnings) = plan_verification(
        project_root,
        &changed_paths,
        &impacted_files,
        &candidates,
        graph.truncated,
        analysis_partial,
        plan_items,
    );
    change_warnings.append(&mut verify_warnings);

    let status = if analysis_partial || graph.truncated {
        AnalysisStatus::Partial
    } else {
        AnalysisStatus::Complete
    };
    let response = ImpactAnalysisResponse {
        ok: true,
        analysis_status: status,
        changed_files: changed_paths,
        impacted_files,
        graph,
        candidate_tests: candidates,
        verification,
        warnings: change_warnings,
        truncated: None,
    };
    apply_budget(response, budget)
}

/// Enforce the response budget deterministically.
///
/// Priority kept: status -> changed files -> verification -> warnings ->
/// impact summary -> graph edges. Low-priority details are dropped first;
/// verification recommendations and incomplete/truncated warnings are never
/// dropped silently. The output stays valid JSON and reports truncation.
fn apply_budget(mut response: ImpactAnalysisResponse, budget: usize) -> ImpactAnalysisResponse {
    // Warnings about incompleteness are load-bearing: never drop them.
    // Other warnings yield first when the budget is tight.
    let fits = |r: &ImpactAnalysisResponse| {
        serde_json::to_string(r)
            .map(|s| s.chars().count() <= budget)
            .unwrap_or(false)
    };
    if fits(&response) {
        return response;
    }
    // 1. Drop graph edges beyond a small head (keep nodes for file summary).
    while !fits(&response) && response.graph.edges.len() > 20 {
        let keep = response.graph.edges.len().saturating_sub(20).max(1);
        response
            .graph
            .edges
            .truncate(response.graph.edges.len() - keep);
        response.graph.truncated = true;
    }
    // 2. Drop candidate detail beyond the head.
    while !fits(&response) && response.candidate_tests.len() > 5 {
        response.candidate_tests.pop();
    }
    // 3. Drop graph nodes beyond the head (keep file lists separately).
    while !fits(&response) && response.graph.nodes.len() > 30 {
        response.graph.nodes.pop();
    }
    // 4. Drop non-critical warnings (keep truncation/incomplete/unknown).
    while !fits(&response) {
        let pos = response.warnings.iter().position(|w| {
            !(w.contains("truncat")
                || w.contains("incomplete")
                || w.contains("unknown")
                || w.contains("Unknown")
                || w.contains("broader")
                || w.contains("Broader"))
        });
        match pos {
            Some(i) => {
                response.warnings.remove(i);
            }
            None => break,
        }
    }
    // 5. As a last resort, clear edges entirely (nodes keep the summary).
    if !fits(&response) {
        response.graph.edges.clear();
        response.graph.truncated = true;
    }
    if !fits(&response) {
        response.graph.nodes.clear();
    }
    let truncated = !fits(&response) || response.graph.truncated;
    if response.graph.truncated
        && !response
            .warnings
            .iter()
            .any(|w| w.contains("response budget"))
    {
        response.warnings.push(
            "Response exceeded response_budget_chars; low-priority graph/test details were removed. Verification recommendations and incompleteness warnings were preserved."
                .to_string(),
        );
    }
    response.truncated = Some(
        truncated
            || response
                .warnings
                .iter()
                .any(|w| w.contains("response budget")),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};

    fn sym(name: &str, file: &str) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::Function,
            file: std::path::PathBuf::from(file),
            start_line: 1,
            start_col: 1,
            end_line: 2,
            end_col: 1,
            parent: None,
            file_total_lines: 10,
            function_lines: Some(2),
            keywords: Vec::new(),
        }
    }

    #[test]
    fn empty_changes_yield_no_changes_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response =
            analyze_from_collected(dir.path(), None, None, vec![], vec![], 2, true, 6000, &[]);
        assert!(response.ok);
        assert_eq!(response.analysis_status, AnalysisStatus::NoChanges);
        assert!(response.verification.is_empty());
    }

    #[test]
    fn missing_repomap_is_partial_with_broad_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").expect("write");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("write");
        let changed = vec![ChangedFile {
            path: "a.rs".to_string(),
            change_kind: ChangeKind::Modified,
        }];
        let response =
            analyze_from_collected(dir.path(), None, None, changed, vec![], 2, true, 6000, &[]);
        assert_eq!(response.analysis_status, AnalysisStatus::Partial);
        assert!(response.warnings.iter().any(|w| w.contains("RepoMap")));
        assert!(response.verification.iter().any(|r| r.program == "cargo"));
    }

    #[test]
    fn large_graph_is_budgeted_without_losing_verification() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("write");
        // 60 files each with a symbol; one changed file calls many.
        let mut symbols = Vec::new();
        let mut relations = Vec::new();
        for i in 0..60 {
            let f = dir.path().join(format!("f{i}.rs"));
            std::fs::write(&f, "fn x() {}\n").expect("write");
            symbols.push(sym(&format!("s{i}"), f.to_str().unwrap()));
        }
        for i in 1..60 {
            relations.push(crate::analysis::SymbolRelation {
                source_symbol_name: format!("s{i}"),
                source_symbol_parent: None,
                source_file_path: dir.path().join(format!("f{i}.rs")),
                target_symbol_name: "s0".to_string(),
                relation_type: crate::analysis::RelationType::Call,
                line: 1,
            });
        }
        let map = RepoMap { symbols, relations };
        let index = crate::analysis::SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![ChangedFile {
            path: "f0.rs".to_string(),
            change_kind: ChangeKind::Modified,
        }];
        let response = analyze_from_collected(
            dir.path(),
            Some(&map),
            Some(&index),
            changed,
            vec![],
            2,
            true,
            6000,
            &[],
        );
        let serialized = serde_json::to_string(&response).expect("json");
        assert!(serialized.chars().count() <= 6000);
        assert!(!response.verification.is_empty());
        // Truncation is reported, never silent.
        if response.graph.truncated {
            assert!(
                response
                    .warnings
                    .iter()
                    .any(|w| w.contains("budget") || w.contains("truncat"))
            );
        }
    }

    #[test]
    fn synthetic_benchmark_reports_sizes() {
        use std::time::Instant;
        // Representative traversal sizes (not performance guarantees).
        // Measures traversal time + visited nodes/edges for a linear chain.
        for size in [1_000usize, 10_000usize] {
            let dir = tempfile::tempdir().expect("tempdir");
            let files: Vec<String> = (0..size)
                .map(|i| {
                    dir.path()
                        .join(format!("f{i}.rs"))
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            let symbols: Vec<SymbolInfo> = (0..size)
                .map(|i| sym(&format!("s{i}"), &files[i]))
                .collect();
            let relations: Vec<crate::analysis::SymbolRelation> = (1..size)
                .map(|i| crate::analysis::SymbolRelation {
                    source_symbol_name: format!("s{i}"),
                    source_symbol_parent: None,
                    source_file_path: std::path::PathBuf::from(&files[i]),
                    target_symbol_name: format!("s{}", i - 1),
                    relation_type: crate::analysis::RelationType::Call,
                    line: 1,
                })
                .collect();
            // Only materialize files for the seed; traversal itself uses the
            // in-memory map (no repeated source reads).
            std::fs::write(dir.path().join("f0.rs"), "fn x() {}\n").expect("write");
            let map = RepoMap { symbols, relations };
            let index =
                crate::analysis::SymbolIdentityIndex::build(&map, dir.path()).expect("index");
            let changed = vec![ChangedFile {
                path: "f0.rs".to_string(),
                change_kind: ChangeKind::Modified,
            }];
            // Seed from the head (s0 is called by s1, s1 by s2, ...);
            // traversal walks incoming callers up to max_depth.
            let tail_changed = vec![ChangedSymbol {
                symbol_id: None,
                file: "f0.rs".to_string(),
                name: "s0".to_string(),
                identification_confidence: IdentificationConfidence::FileLevel,
            }];
            let _ = changed; // changed-file list unused here; seed above drives traversal
            let start = Instant::now();
            let (graph, _) = build_impact_graph(
                dir.path(),
                &map,
                Some(&index),
                &tail_changed,
                5,
                MAX_NODES,
                MAX_EDGES,
            );
            let elapsed = start.elapsed();
            // Report sizes; no hard time bound (CI machines vary).
            eprintln!(
                "benchmark size={size} nodes={} edges={} truncated={} elapsed={elapsed:?}",
                graph.nodes.len(),
                graph.edges.len(),
                graph.truncated
            );
            assert!(!graph.nodes.is_empty());
            assert!(graph.nodes.len() <= MAX_NODES);
            assert!(graph.edges.len() <= MAX_EDGES);
        }
    }
}
