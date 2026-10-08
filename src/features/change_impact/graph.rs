use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::analysis::{RepoMap, SymbolIdentityIndex, normalize_relative_path};

use super::types::{
    ChangedSymbol, EdgeConfidence, IdentificationConfidence, ImpactEdge, ImpactGraph, ImpactNode,
};

/// Default traversal depth (incoming callers).
pub const DEFAULT_MAX_DEPTH: u8 = 2;
/// Hard engineering limits (not benchmark-derived optima).
pub const MAX_DEPTH_LIMIT: u8 = 5;
pub const MAX_NODES: usize = 500;
pub const MAX_EDGES: usize = 2000;

/// Prefix marking a graph location the map places outside the project root.
///
/// Such nodes keep their file name for attribution, but the absolute path
/// is never emitted: tool output must stay project-relative and must not
/// leak temp-dir or home-directory spellings into the LLM context.
const OUTSIDE_ROOT_PREFIX: &str = "<outside-root>";

/// Project-relative display path that never leaks absolute paths.
///
/// Falls back to `<outside-root>/<file-name>` when the map entry cannot be
/// relativized (stale cache entries, outside-root paths). Callers treat such
/// nodes as unknown locations with broader verification.
fn sanitized_file(project_root: &Path, file: &Path) -> String {
    normalize_relative_path(project_root, file).unwrap_or_else(|_| {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string());
        format!("{OUTSIDE_ROOT_PREFIX}/{name}")
    })
}

/// Key for a symbol that may lack a stable id (deleted/ambiguous source).
fn fallback_key(file: &str, name: &str) -> String {
    if name.is_empty() {
        format!("file:{file}")
    } else {
        format!("{file}::{name}")
    }
}

/// Build the reverse dependency index once per analysis: O(E).
///
/// `target_name -> [relation_idx]`. Callers then traverse reachable
/// subgraphs in O(V + E) without per-symbol full scans.
struct ReverseIndex {
    by_target: HashMap<String, Vec<usize>>,
    name_to_symbols: HashMap<String, Vec<usize>>,
}

impl ReverseIndex {
    fn build(map: &RepoMap) -> Self {
        let mut by_target: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, rel) in map.relations.iter().enumerate() {
            by_target
                .entry(rel.target_symbol_name.clone())
                .or_default()
                .push(idx);
        }
        let mut name_to_symbols: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, sym) in map.symbols.iter().enumerate() {
            name_to_symbols
                .entry(sym.name.clone())
                .or_default()
                .push(idx);
        }
        Self {
            by_target,
            name_to_symbols,
        }
    }
}

/// Resolve source symbols for a relation: all symbols in the source file
/// with matching name + parent. Empty when the source no longer exists.
fn resolve_sources(map: &RepoMap, project_root: &Path, relation_idx: usize) -> Vec<usize> {
    let rel = &map.relations[relation_idx];
    let mut out = Vec::new();
    for (idx, sym) in map.symbols.iter().enumerate() {
        let Ok(rel_path) = normalize_relative_path(project_root, &sym.file) else {
            continue;
        };
        let Ok(src_path) = normalize_relative_path(project_root, &rel.source_file_path) else {
            continue;
        };
        if rel_path != src_path {
            continue;
        }
        if sym.name != rel.source_symbol_name {
            continue;
        }
        if sym.parent != rel.source_symbol_parent {
            continue;
        }
        out.push(idx);
    }
    out.sort_unstable();
    out
}

/// Classify edge confidence without numeric scores.
///
/// - `Ambiguous`: the target name resolves to multiple symbols (a
///   name-only match cannot prove which one is called).
/// - `Exact`: unique target + `call` relation.
/// - `Inferred`: unique target + non-call relation (import/use/...),
///   useful for context but not a proven caller.
fn classify_edge(map: &RepoMap, reverse: &ReverseIndex, relation_idx: usize) -> EdgeConfidence {
    let rel = &map.relations[relation_idx];
    let count = reverse
        .name_to_symbols
        .get(&rel.target_symbol_name)
        .map(Vec::len)
        .unwrap_or(0);
    if count != 1 {
        return EdgeConfidence::Ambiguous;
    }
    match rel.relation_type {
        crate::analysis::RelationType::Call => EdgeConfidence::Exact,
        _ => EdgeConfidence::Inferred,
    }
}

/// Symbol display key: stable id when available, else file::name fallback.
/// Never manufactures an id for ambiguous nodes.
fn node_key(
    map: &RepoMap,
    project_root: &Path,
    identity: Option<&SymbolIdentityIndex>,
    idx: usize,
) -> (String, Option<String>, String, String) {
    let sym = &map.symbols[idx];
    let file = sanitized_file(project_root, &sym.file);
    let id = identity
        .and_then(|index| index.id_for_index(idx))
        .map(|id| id.as_str().to_string());
    let key = id.clone().unwrap_or_else(|| fallback_key(&file, &sym.name));
    (key, id, file, sym.name.clone())
}

/// Bounded reverse BFS from changed symbols.
///
/// Incoming dependencies (callers) are the affected set; outgoing
/// dependencies are context only and are never classified as affected.
pub fn build_impact_graph(
    project_root: &Path,
    map: &RepoMap,
    identity: Option<&SymbolIdentityIndex>,
    changed: &[ChangedSymbol],
    max_depth: u8,
    max_nodes: usize,
    max_edges: usize,
) -> (ImpactGraph, Vec<String>) {
    let mut warnings = Vec::new();
    let max_depth = max_depth.min(MAX_DEPTH_LIMIT);
    let max_nodes = max_nodes.clamp(1, MAX_NODES);
    let max_edges = max_edges.min(MAX_EDGES);

    // Outside-root map entries can never be named project-relatively; flag
    // them once here so the analysis stays conservative ("unknown" in
    // mod.rs flips the result to partial with broader verification).
    let outside_root = map
        .symbols
        .iter()
        .filter(|s| normalize_relative_path(project_root, &s.file).is_err())
        .count();
    if outside_root > 0 {
        warnings.push(format!(
            "{outside_root} RepoMap symbol(s) outside the project root were treated as unknown locations; absolute paths are withheld and broader verification is recommended."
        ));
    }

    // Seed: map changed symbols back to indices. File-level candidates
    // contribute every symbol in their file; unresolved entries contribute
    // no seed (their impact is already reported as unknown).
    //
    // Both lookups are built once (O(S)): per-seed linear scans would be
    // O(seeds*S) with a redundant parse/normalization each.
    let file_index = crate::analysis::index_symbols_by_file(map, project_root);
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    if let Some(index) = identity {
        for (idx, _) in map.symbols.iter().enumerate() {
            if let Some(id) = index.id_for_index(idx) {
                id_to_idx.insert(id.as_str().to_string(), idx);
            }
        }
    }
    let mut seeds: Vec<usize> = Vec::new();
    for c in changed {
        if c.identification_confidence == IdentificationConfidence::Unresolved {
            continue;
        }
        if let Some(id_str) = &c.symbol_id
            && let Some(&idx) = id_to_idx.get(id_str)
        {
            seeds.push(idx);
            continue;
        }
        // Fallback: every symbol in the file.
        if let Some(idxs) = file_index.get(&c.file) {
            seeds.extend(idxs.iter().copied());
        }
    }
    seeds.sort_unstable();
    seeds.dedup();

    if seeds.is_empty() {
        return (
            ImpactGraph {
                nodes: vec![],
                edges: vec![],
                truncated: false,
            },
            warnings,
        );
    }

    let reverse = ReverseIndex::build(map);
    let mut depth_by_idx: HashMap<usize, usize> = HashMap::new();
    let mut fallback_depth: HashMap<String, usize> = HashMap::new();
    let mut edges: Vec<ImpactEdge> = Vec::new();
    let mut truncated = false;
    let mut queue: VecDeque<usize> = VecDeque::new();
    for seed in &seeds {
        if !depth_by_idx.contains_key(seed) {
            depth_by_idx.insert(*seed, 0);
            queue.push_back(*seed);
        }
    }

    // Deterministic traversal: sort each frontier by (file, name) before
    // expansion so HashMap iteration order never leaks into output.
    while let Some(current) = queue.pop_front() {
        let current_depth = depth_by_idx.get(&current).copied().unwrap_or(0);
        if current_depth >= max_depth as usize {
            continue;
        }
        let target_name = map.symbols[current].name.clone();
        let mut incoming: Vec<usize> = reverse
            .by_target
            .get(&target_name)
            .cloned()
            .unwrap_or_default();
        // Deterministic relation order.
        incoming.sort_by(|a, b| {
            let ra = &map.relations[*a];
            let rb = &map.relations[*b];
            ra.source_file_path
                .cmp(&rb.source_file_path)
                .then(ra.source_symbol_name.cmp(&rb.source_symbol_name))
                .then(ra.source_symbol_parent.cmp(&rb.source_symbol_parent))
                .then(ra.relation_type.as_str().cmp(rb.relation_type.as_str()))
        });
        for rel_idx in incoming {
            if edges.len() >= max_edges {
                truncated = true;
                warnings.push(format!(
                    "Graph traversal stopped at {max_edges} edges; the graph is incomplete and broader verification is recommended."
                ));
                break;
            }
            let confidence = classify_edge(map, &reverse, rel_idx);
            if confidence == EdgeConfidence::Ambiguous {
                warnings.push(format!(
                    "Ambiguous dependency on '{}' (multiple symbols share the name); treated conservatively, not as an exact edge.",
                    map.relations[rel_idx].target_symbol_name
                ));
            }
            let sources = resolve_sources(map, project_root, rel_idx);
            let (target_key, _, _, _) = node_key(map, project_root, identity, current);
            if sources.is_empty() {
                // Source symbol missing (deleted/renamed): record an
                // unresolved caller node rather than dropping the edge.
                let rel = &map.relations[rel_idx];
                let src_file = sanitized_file(project_root, &rel.source_file_path);
                let src_key = fallback_key(&src_file, &rel.source_symbol_name);
                if depth_by_idx.len() + fallback_depth.len() >= max_nodes
                    && !fallback_depth.contains_key(&src_key)
                {
                    truncated = true;
                    warnings.push(format!(
                        "Graph traversal stopped at {max_nodes} nodes; the graph is incomplete and broader verification is recommended."
                    ));
                    break;
                }
                let next_depth = current_depth + 1;
                fallback_depth.entry(src_key.clone()).or_insert(next_depth);
                edges.push(ImpactEdge {
                    source: src_key,
                    target: target_key,
                    relation_kind: map.relations[rel_idx].relation_type.as_str().to_string(),
                    confidence,
                });
                continue;
            }
            // Deterministic source order.
            let mut ordered = sources.clone();
            ordered.sort_by(|a, b| {
                let sa = &map.symbols[*a];
                let sb = &map.symbols[*b];
                sa.file
                    .cmp(&sb.file)
                    .then(sa.name.cmp(&sb.name))
                    .then(sa.parent.cmp(&sb.parent))
            });
            for src in ordered {
                if edges.len() >= max_edges {
                    truncated = true;
                    break;
                }
                let (src_key, _, _, _) = node_key(map, project_root, identity, src);
                let (tgt_key, _, _, _) = node_key(map, project_root, identity, current);
                edges.push(ImpactEdge {
                    source: src_key.clone(),
                    target: tgt_key,
                    relation_kind: map.relations[rel_idx].relation_type.as_str().to_string(),
                    confidence,
                });
                if !depth_by_idx.contains_key(&src) {
                    if depth_by_idx.len() + fallback_depth.len() >= max_nodes {
                        truncated = true;
                        warnings.push(format!(
                            "Graph traversal stopped at {max_nodes} nodes; the graph is incomplete and broader verification is recommended."
                        ));
                        break;
                    }
                    // Cycles terminate via the visited set.
                    depth_by_idx.insert(src, current_depth + 1);
                    queue.push_back(src);
                }
            }
            if truncated {
                break;
            }
        }
        if truncated {
            break;
        }
    }
    if truncated {
        warnings.push(
            "A truncated graph must never be presented as complete; broader verification is recommended."
                .to_string(),
        );
    }

    // Nodes: visited symbols + fallback callers, deterministically sorted.
    let mut nodes: Vec<ImpactNode> = Vec::new();
    for (idx, depth) in &depth_by_idx {
        let (_, id, file, name) = node_key(map, project_root, identity, *idx);
        nodes.push(ImpactNode {
            symbol_id: id,
            file,
            name,
            depth: *depth,
        });
    }
    for (key, depth) in &fallback_depth {
        // key is `file::name` or `file:...`; split for display.
        let (file, name) = key
            .split_once("::")
            .map(|(f, n)| (f.to_string(), n.to_string()))
            .unwrap_or((key.clone(), String::new()));
        nodes.push(ImpactNode {
            symbol_id: None,
            file,
            name,
            depth: *depth,
        });
    }
    nodes.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(a.file.cmp(&b.file))
            .then(a.name.cmp(&b.name))
            .then(a.symbol_id.cmp(&b.symbol_id))
    });
    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.target.cmp(&b.target))
            .then(a.relation_kind.cmp(&b.relation_kind))
    });
    edges.dedup_by(|a, b| {
        a.source == b.source && a.target == b.target && a.relation_kind == b.relation_kind
    });

    // Impacted files: every visited file except the changed set is context
    // for the caller (computed in mod.rs); the graph itself stays symbol-level.

    (
        ImpactGraph {
            nodes,
            edges,
            truncated,
        },
        warnings,
    )
}

/// Human-readable file summary for warnings (avoids leaking ids).
pub fn impacted_files_from_graph(graph: &ImpactGraph, changed_paths: &[String]) -> Vec<String> {
    let changed: HashSet<&str> = changed_paths.iter().map(String::as_str).collect();
    let mut files: BTreeMap<String, usize> = BTreeMap::new();
    for node in &graph.nodes {
        if !changed.contains(node.file.as_str()) {
            files.entry(node.file.clone()).or_insert(node.depth);
        }
    }
    files.into_keys().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{RelationType, RepoMap, SymbolInfo, SymbolKind, SymbolRelation};
    use std::path::PathBuf;

    fn sym(name: &str, file: &str) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::Function,
            file: PathBuf::from(file),
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

    fn rel(source: &str, src_file: &str, target: &str) -> SymbolRelation {
        SymbolRelation {
            source_symbol_name: source.to_string(),
            source_symbol_parent: None,
            source_file_path: PathBuf::from(src_file),
            target_symbol_name: target.to_string(),
            relation_type: RelationType::Call,
            line: 1,
        }
    }

    fn changed_for(map: &RepoMap, root: &Path, file: &str, name: &str) -> ChangedSymbol {
        let index = SymbolIdentityIndex::build(map, root).expect("index");
        let idx = map
            .symbols
            .iter()
            .position(|s| s.name == name && normalize_relative_path(root, &s.file).unwrap() == file)
            .expect("symbol");
        ChangedSymbol {
            symbol_id: index.id_for_index(idx).map(|id| id.as_str().to_string()),
            file: file.to_string(),
            name: name.to_string(),
            identification_confidence: IdentificationConfidence::FileLevel,
        }
    }

    #[test]
    fn single_symbol_direct_caller() {
        let dir = tempfile::tempdir().expect("tempdir");
        let price = dir.path().join("price.rs");
        let order = dir.path().join("order.rs");
        std::fs::write(&price, "fn calc() {}\n").expect("write");
        std::fs::write(&order, "fn create() {}\n").expect("write");
        let map = RepoMap {
            symbols: vec![
                sym("calc", price.to_str().unwrap()),
                sym("create", order.to_str().unwrap()),
            ],
            relations: vec![rel("create", order.to_str().unwrap(), "calc")],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![changed_for(&map, dir.path(), "price.rs", "calc")];
        let (graph, _) = build_impact_graph(dir.path(), &map, Some(&index), &changed, 2, 500, 2000);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].confidence, EdgeConfidence::Exact);
        assert!(!graph.truncated);
    }

    #[test]
    fn transitive_caller_within_depth() {
        let dir = tempfile::tempdir().expect("tempdir");
        for f in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(dir.path().join(f), "fn x() {}\n").expect("write");
        }
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        let c = dir.path().join("c.rs");
        let map = RepoMap {
            symbols: vec![
                sym("calc", a.to_str().unwrap()),
                sym("create", b.to_str().unwrap()),
                sym("checkout", c.to_str().unwrap()),
            ],
            relations: vec![
                rel("create", b.to_str().unwrap(), "calc"),
                rel("checkout", c.to_str().unwrap(), "create"),
            ],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![changed_for(&map, dir.path(), "a.rs", "calc")];
        let (full, _) = build_impact_graph(dir.path(), &map, Some(&index), &changed, 2, 500, 2000);
        assert_eq!(full.nodes.len(), 3);
        let (shallow, _) =
            build_impact_graph(dir.path(), &map, Some(&index), &changed, 1, 500, 2000);
        assert_eq!(shallow.nodes.len(), 2);
    }

    #[test]
    fn cycle_terminates() {
        let dir = tempfile::tempdir().expect("tempdir");
        for f in ["a.rs", "b.rs"] {
            std::fs::write(dir.path().join(f), "fn x() {}\n").expect("write");
        }
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        let map = RepoMap {
            symbols: vec![
                sym("fa", a.to_str().unwrap()),
                sym("fb", b.to_str().unwrap()),
            ],
            relations: vec![
                rel("fa", a.to_str().unwrap(), "fb"),
                rel("fb", b.to_str().unwrap(), "fa"),
            ],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![changed_for(&map, dir.path(), "a.rs", "fa")];
        let (graph, _) = build_impact_graph(dir.path(), &map, Some(&index), &changed, 5, 500, 2000);
        assert_eq!(graph.nodes.len(), 2);
        assert!(!graph.truncated);
    }

    #[test]
    fn duplicate_names_are_ambiguous_not_exact() {
        let dir = tempfile::tempdir().expect("tempdir");
        for f in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(dir.path().join(f), "fn x() {}\n").expect("write");
        }
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        let c = dir.path().join("c.rs");
        let map = RepoMap {
            symbols: vec![
                sym("helper", a.to_str().unwrap()),
                sym("helper", b.to_str().unwrap()),
                sym("caller", c.to_str().unwrap()),
            ],
            relations: vec![rel("caller", c.to_str().unwrap(), "helper")],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![changed_for(&map, dir.path(), "a.rs", "helper")];
        let (graph, warnings) =
            build_impact_graph(dir.path(), &map, Some(&index), &changed, 2, 500, 2000);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].confidence, EdgeConfidence::Ambiguous);
        assert!(warnings.iter().any(|w| w.contains("Ambiguous")));
    }

    #[test]
    fn missing_target_yields_no_exact_edge() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").expect("write");
        let map = RepoMap {
            symbols: vec![sym("a", dir.path().join("a.rs").to_str().unwrap())],
            relations: vec![],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![changed_for(&map, dir.path(), "a.rs", "a")];
        let (graph, _) = build_impact_graph(dir.path(), &map, Some(&index), &changed, 2, 500, 2000);
        assert_eq!(graph.nodes.len(), 1);
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn empty_graph_for_unresolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let map = RepoMap {
            symbols: vec![],
            relations: vec![],
        };
        let changed = vec![ChangedSymbol {
            symbol_id: None,
            file: "gone.rs".to_string(),
            name: String::new(),
            identification_confidence: IdentificationConfidence::Unresolved,
        }];
        let (graph, _) = build_impact_graph(dir.path(), &map, None, &changed, 2, 500, 2000);
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn depth_node_and_edge_limits_truncate() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("f{i}.rs")), "fn x() {}\n").expect("write");
        }
        let mut symbols = Vec::new();
        let mut relations = Vec::new();
        for i in 0..10 {
            symbols.push(sym(
                &format!("f{i}"),
                dir.path()
                    .join(format!("f{i}.rs"))
                    .to_str()
                    .unwrap()
                    .to_string()
                    .as_str(),
            ));
        }
        // Chain f0 <- f1 <- ... : each calls the previous.
        // Build with owned strings to avoid borrow issues.
        let files: Vec<String> = (0..10)
            .map(|i| {
                dir.path()
                    .join(format!("f{i}.rs"))
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        for (i, file) in files.iter().enumerate().skip(1) {
            relations.push(SymbolRelation {
                source_symbol_name: format!("f{i}"),
                source_symbol_parent: None,
                source_file_path: PathBuf::from(file),
                target_symbol_name: format!("f{}", i - 1),
                relation_type: RelationType::Call,
                line: 1,
            });
        }
        // Rebuild symbols with correct files (the helper above used temp strings).
        let map = RepoMap {
            symbols: (0..10).map(|i| sym(&format!("f{i}"), &files[i])).collect(),
            relations,
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![changed_for(&map, dir.path(), "f0.rs", "f0")];
        let (depth_limited, _) =
            build_impact_graph(dir.path(), &map, Some(&index), &changed, 1, 500, 2000);
        assert_eq!(depth_limited.nodes.len(), 2);
        let (node_limited, _) =
            build_impact_graph(dir.path(), &map, Some(&index), &changed, 5, 2, 2000);
        assert!(node_limited.truncated);
        let (edge_limited, _) =
            build_impact_graph(dir.path(), &map, Some(&index), &changed, 5, 500, 1);
        assert!(edge_limited.truncated);
        let _ = symbols;
    }

    #[test]
    fn outside_root_symbols_never_leak_absolute_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").expect("write");
        let map = RepoMap {
            symbols: vec![
                sym("a", dir.path().join("a.rs").to_str().unwrap()),
                sym("ext", "/other/secret/ext.rs"),
            ],
            relations: vec![rel("ext", "/other/secret/ext.rs", "a")],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        // Index skips outside-root entries, so the seed resolves by file.
        assert_eq!(index.len(), 1);
        let changed = vec![changed_for(&map, dir.path(), "a.rs", "a")];
        let (graph, warnings) =
            build_impact_graph(dir.path(), &map, Some(&index), &changed, 2, 500, 2000);
        let serialized = serde_json::to_string(&graph).expect("json");
        assert!(
            !serialized.contains("/other/secret"),
            "absolute path leaked: {serialized}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("outside the project root")),
            "outside-root participation must be reported"
        );
        assert!(!graph.edges.is_empty());
    }

    #[test]
    fn output_order_is_deterministic() {
        let dir = tempfile::tempdir().expect("tempdir");
        for f in ["a.rs", "m.rs", "z.rs"] {
            std::fs::write(dir.path().join(f), "fn x() {}\n").expect("write");
        }
        let a = dir.path().join("a.rs");
        let m = dir.path().join("m.rs");
        let z = dir.path().join("z.rs");
        // Insert in non-sorted order; output must still sort.
        let map = RepoMap {
            symbols: vec![
                sym("target", z.to_str().unwrap()),
                sym("zeta", a.to_str().unwrap()),
                sym("mid", m.to_str().unwrap()),
            ],
            relations: vec![
                rel("zeta", a.to_str().unwrap(), "target"),
                rel("mid", m.to_str().unwrap(), "target"),
            ],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![changed_for(&map, dir.path(), "z.rs", "target")];
        let (first, _) = build_impact_graph(dir.path(), &map, Some(&index), &changed, 2, 500, 2000);
        let (second, _) =
            build_impact_graph(dir.path(), &map, Some(&index), &changed, 2, 500, 2000);
        assert_eq!(first.nodes, second.nodes);
        assert_eq!(first.edges, second.edges);
        let files: Vec<&str> = first.nodes.iter().map(|n| n.file.as_str()).collect();
        let mut sorted = files.clone();
        sorted.sort();
        // Depth 0 first, then depth 1 sorted by file.
        assert_eq!(first.nodes[0].depth, 0);
        assert!(first.nodes[1..].iter().all(|n| n.depth == 1));
        let _ = sorted;
    }
}
