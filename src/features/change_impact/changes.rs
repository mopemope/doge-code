use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::analysis::{RepoMap, SymbolIdentityIndex, normalize_relative_path};

use super::types::{ChangeKind, ChangedFile, ChangedSymbol, IdentificationConfidence};

/// Upper bound for explicit path inputs (fail closed, deterministic).
pub const MAX_EXPLICIT_PATHS: usize = 200;

/// Result of collecting changed files.
#[derive(Debug, Clone)]
pub struct CollectedChanges {
    pub files: Vec<ChangedFile>,
    pub warnings: Vec<String>,
}

/// Normalize one user-supplied path to a project-relative `/` form.
///
/// Returns `None` for outside-root / invalid spellings (caller records a
/// warning and skips the path rather than guessing).
fn normalize_input_path(project_root: &Path, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject NUL and other control-implied issues early; JSON strings are
    // UTF-8 so non-UTF-8 filesystem names cannot be expressed here.
    if trimmed.contains('\0') {
        return None;
    }
    let input_path = Path::new(trimmed);
    // Candidate absolute path: join relative spellings onto the root so
    // `..` and symlink components resolve against the real filesystem.
    let candidate_abs: PathBuf = if input_path.is_absolute() {
        input_path.to_path_buf()
    } else {
        // Reject `..` escapes lexically before touching the filesystem so
        // `sub/../../outside` never resolves to an in-root alias.
        let mut depth: i64 = 0;
        for comp in input_path.components() {
            match comp {
                Component::ParentDir => {
                    depth -= 1;
                    if depth < 0 {
                        return None;
                    }
                }
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        project_root.join(input_path)
    };
    // Canonical scope check: resolves symlinks, denies escapes. Missing
    // targets resolve via the nearest existing ancestor.
    let canonical_target = crate::tools::scope::canonicalize_target(&candidate_abs)?;
    let canonical_root = crate::tools::scope::canonicalize_root(project_root);
    if !canonical_target.starts_with(&canonical_root) {
        return None;
    }
    // Project-relative output: prefer the canonical relativization, fall
    // back to lexical for missing files whose ancestor check passed.
    if let Ok(rel) = canonical_target.strip_prefix(&canonical_root) {
        let mut parts: Vec<String> = Vec::new();
        for comp in rel.components() {
            match comp {
                Component::Normal(os) => parts.push(os.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir => return None,
                Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        if parts.is_empty() {
            return None;
        }
        return Some(parts.join("/"));
    }
    // Fallback: lexical relativization of the original spelling (covers
    // deleted files whose canonical ancestor is in-root but whose exact
    // spelling cannot be re-anchored).
    normalize_relative_path(project_root, &candidate_abs)
        .ok()
        .or_else(|| {
            // Last resort: clean the raw relative spelling directly.
            let rel = Path::new(trimmed);
            if rel.is_absolute() {
                return None;
            }
            let mut parts: Vec<String> = Vec::new();
            for comp in rel.components() {
                match comp {
                    Component::Normal(os) => parts.push(os.to_string_lossy().into_owned()),
                    Component::CurDir => {}
                    Component::ParentDir => return None,
                    _ => return None,
                }
            }
            if parts.is_empty() {
                return None;
            }
            Some(parts.join("/"))
        })
}

/// Classify one project-relative file against the workspace + repomap.
fn classify_file(project_root: &Path, rel: &str, repomap: Option<&RepoMap>) -> ChangedFile {
    let abs = project_root.join(rel);
    let exists = abs.symlink_metadata().is_ok();
    if !exists {
        return ChangedFile {
            path: rel.to_string(),
            change_kind: ChangeKind::Deleted,
        };
    }
    // Symlink escaping the root was already denied; a symlink to an
    // in-root target is a normal file for analysis.
    if let Some(map) = repomap {
        let has_symbols = map.symbols.iter().any(|s| {
            normalize_relative_path(project_root, &s.file)
                .map(|r| r == rel)
                .unwrap_or(false)
        });
        if has_symbols {
            return ChangedFile {
                path: rel.to_string(),
                change_kind: ChangeKind::Modified,
            };
        }
        // Exists but unrepresented: new file, unsupported language, or
        // stale map. `Added` is honest; the graph step will mark impact
        // unknown rather than inventing relations.
        return ChangedFile {
            path: rel.to_string(),
            change_kind: ChangeKind::Added,
        };
    }
    // No map: cannot distinguish added vs modified.
    ChangedFile {
        path: rel.to_string(),
        change_kind: ChangeKind::Modified,
    }
}

/// Collect explicit user-supplied paths.
///
/// - Deduplicated + deterministically ordered.
/// - Outside-root / invalid paths are skipped with a warning (never guessed).
/// - Deleted files are kept as `Deleted` (never silently dropped).
pub fn collect_explicit_changes(
    project_root: &Path,
    paths: &[String],
    repomap: Option<&RepoMap>,
) -> CollectedChanges {
    let mut warnings = Vec::new();
    if paths.len() > MAX_EXPLICIT_PATHS {
        warnings.push(format!(
            "Only the first {MAX_EXPLICIT_PATHS} of {} paths were analyzed; narrow the change set for complete coverage.",
            paths.len()
        ));
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut invalid = 0usize;
    for raw in paths.iter().take(MAX_EXPLICIT_PATHS) {
        match normalize_input_path(project_root, raw) {
            Some(rel) => {
                seen.insert(rel);
            }
            None => {
                invalid += 1;
            }
        }
    }
    if invalid > 0 {
        warnings.push(format!(
            "{invalid} path(s) were outside the project root or invalid and were excluded; verification falls back to broader coverage."
        ));
    }
    let mut files: Vec<ChangedFile> = seen
        .into_iter()
        .map(|rel| classify_file(project_root, &rel, repomap))
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    CollectedChanges { files, warnings }
}

/// Collect the current session's active tracked changes (read-only).
///
/// - Only `Active` provenance states contribute; superseded / diverged /
///   missing / reverted states are never treated as active.
/// - Session `changed_files` entries with no active provenance record are
///   included as `Unknown` (untracked activity) so they are not silently
///   dropped; the planner treats them conservatively.
/// - Never scans git history and never diffs against an arbitrary branch.
pub fn collect_active_changes(
    project_root: &Path,
    events: &[crate::provenance::ProvenanceEventEnvelope],
    session_changed_files: &[String],
    repomap: Option<&RepoMap>,
) -> CollectedChanges {
    let mut warnings = Vec::new();
    let resolved = crate::provenance::query::resolve_active_states(project_root, events);
    let mut active_files: BTreeSet<String> = BTreeSet::new();
    let mut non_active = 0usize;
    for r in &resolved {
        match r.state {
            crate::provenance::query::ActiveChangeState::Active => {
                // Provenance stores project-relative `/` paths already.
                let cleaned = r.file.trim().trim_start_matches("./").replace('\\', "/");
                if !cleaned.is_empty() && !cleaned.contains("..") {
                    active_files.insert(cleaned);
                }
            }
            _ => non_active += 1,
        }
    }
    if non_active > 0 {
        warnings.push(format!(
            "{non_active} historical change record(s) (superseded/diverged/missing/reverted) were excluded; only active changes were analyzed."
        ));
    }
    // Untracked session activity: visible but without provenance evidence.
    let mut untracked = 0usize;
    let mut invalid_session = 0usize;
    for raw in session_changed_files {
        let cleaned = raw.trim().trim_start_matches("./").replace('\\', "/");
        if cleaned.is_empty() || cleaned.contains("..") {
            invalid_session += 1;
            continue;
        }
        if !active_files.contains(&cleaned) {
            // Validate against the root so `..` escapes cannot sneak in via
            // session state; invalid entries are reported, not analyzed.
            if normalize_input_path(project_root, &cleaned).is_some() {
                active_files.insert(cleaned);
                untracked += 1;
            } else {
                warnings.push(format!(
                    "Session entry '{cleaned}' is outside the project root and was excluded."
                ));
            }
        }
    }
    if untracked > 0 {
        warnings.push(format!(
            "{untracked} session file(s) have no active provenance record and were treated as unknown changes; broader verification is recommended."
        ));
    }
    if invalid_session > 0 {
        warnings.push(format!(
            "{invalid_session} session file(s) were invalid or escaped the project root and were excluded; verification falls back to broader coverage."
        ));
    }
    let mut files: Vec<ChangedFile> = active_files
        .into_iter()
        .map(|rel| {
            let mut cf = classify_file(project_root, &rel, repomap);
            // Untracked entries have no evidence for added vs modified;
            // keep the classifier output unless the file is missing.
            if cf.change_kind != ChangeKind::Deleted
                && !resolved.iter().any(|r| {
                    r.state == crate::provenance::query::ActiveChangeState::Active
                        && r.file.trim().trim_start_matches("./").replace('\\', "/") == rel
                })
            {
                cf.change_kind = ChangeKind::Unknown;
            }
            cf
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    CollectedChanges { files, warnings }
}

/// Identify symbols belonging to changed files (read-only).
///
/// Initial version: file-level fallback marks every symbol in a changed
/// file as a `FileLevel` candidate. No certainty is inferred from
/// name matching. Deleted / missing / unrepresented files yield an
/// `Unresolved` entry so the planner recommends broader verification
/// instead of concluding "no impact".
pub fn identify_changed_symbols(
    project_root: &Path,
    repomap: Option<&RepoMap>,
    identity: Option<&SymbolIdentityIndex>,
    changed_files: &[ChangedFile],
) -> (Vec<ChangedSymbol>, Vec<String>) {
    let mut warnings = Vec::new();
    let Some(map) = repomap else {
        warnings.push(
            "RepoMap is unavailable; symbol identification was skipped and impact is unknown. Broader verification is recommended."
                .to_string(),
        );
        let unresolved = changed_files
            .iter()
            .map(|f| ChangedSymbol {
                symbol_id: None,
                file: f.path.clone(),
                name: String::new(),
                identification_confidence: IdentificationConfidence::Unresolved,
            })
            .collect();
        return (unresolved, warnings);
    };
    let mut out: Vec<ChangedSymbol> = Vec::new();
    // File -> symbol lookup built once (O(S)); per-file scans would be
    // O(F*S) with a redundant path normalization per symbol per file.
    let file_index = crate::analysis::index_symbols_by_file(map, project_root);
    for changed in changed_files {
        if changed.change_kind == ChangeKind::Deleted {
            warnings.push(format!(
                "Deleted file '{}' is no longer in the current RepoMap; its impact is unknown and broader verification is recommended.",
                changed.path
            ));
            out.push(ChangedSymbol {
                symbol_id: None,
                file: changed.path.clone(),
                name: String::new(),
                identification_confidence: IdentificationConfidence::Unresolved,
            });
            continue;
        }
        let mut matched: Vec<(usize, &crate::analysis::SymbolInfo)> = file_index
            .get(&changed.path)
            .map(|idxs| idxs.iter().map(|&idx| (idx, &map.symbols[idx])).collect())
            .unwrap_or_default();
        matched.sort_by(|a, b| {
            a.1.name
                .cmp(&b.1.name)
                .then(a.1.start_line.cmp(&b.1.start_line))
                .then(a.1.start_col.cmp(&b.1.start_col))
        });
        if matched.is_empty() {
            // New/unsupported-language file: no symbols to traverse.
            if changed.change_kind == ChangeKind::Added {
                warnings.push(format!(
                    "File '{}' has no symbols in the RepoMap (new or unsupported language); file-level impact only.",
                    changed.path
                ));
            } else {
                warnings.push(format!(
                    "File '{}' has no symbols in the RepoMap; its impact is unknown and broader verification is recommended.",
                    changed.path
                ));
            }
            out.push(ChangedSymbol {
                symbol_id: None,
                file: changed.path.clone(),
                name: String::new(),
                identification_confidence: IdentificationConfidence::Unresolved,
            });
            continue;
        }
        for (idx, sym) in matched.iter() {
            let id = identity
                .and_then(|index| index.id_for_index(*idx))
                .map(|id| id.as_str().to_string());
            out.push(ChangedSymbol {
                symbol_id: id,
                file: changed.path.clone(),
                name: sym.name.clone(),
                identification_confidence: IdentificationConfidence::FileLevel,
            });
        }
        // Stale check (bounded, read-only, no persistent rebuild): parse the
        // current file snapshot and compare symbol names with the shared
        // map. A mismatch means the map is stale for this file; report it
        // as partial rather than presenting incomplete callers as complete.
        // `cached` reuses the already-resolved match so the map is not
        // scanned a second time for this file.
        if changed.change_kind == ChangeKind::Modified {
            let mut cached: Vec<String> = matched
                .iter()
                .map(|(_, s)| format!("{}:{}", s.kind.as_str(), s.name))
                .collect();
            cached.sort();
            cached.dedup();
            if let Some(stale_warning) = detect_stale_file(project_root, &changed.path, &cached) {
                warnings.push(stale_warning);
            }
        }
    }
    out.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.name.cmp(&b.name))
            .then(a.symbol_id.cmp(&b.symbol_id))
    });
    out.dedup_by(|a, b| a.file == b.file && a.name == b.name && a.symbol_id == b.symbol_id);
    (out, warnings)
}

/// Check whether the shared RepoMap is stale for one file.
///
/// Reads the current file once (skipping files over 1 MiB to keep RAM
/// flat), parses the snapshot, and compares symbol name sets against the
/// caller-provided `cached` names (already resolved from the map, so no
/// second map scan). Returns a warning when the workspace disagrees;
/// returns `None` when the file is unparseable, unsupported, or matches
/// (no claim made).
fn detect_stale_file(project_root: &Path, rel: &str, cached: &[String]) -> Option<String> {
    let abs = project_root.join(rel);
    let meta = std::fs::metadata(&abs).ok()?;
    if !meta.is_file() || meta.len() > 1_048_576 {
        return None;
    }
    let source = std::fs::read_to_string(&abs).ok()?;
    let analyzed = crate::analysis::parser::analyze_source(&abs, &source).ok()??;
    let mut fresh: Vec<String> = analyzed
        .repomap
        .symbols
        .iter()
        .map(|s| format!("{}:{}", s.kind.as_str(), s.name))
        .collect();
    fresh.sort();
    fresh.dedup();
    if fresh != cached {
        Some(format!(
            "RepoMap appears stale for '{rel}'; the workspace symbols differ from the cached analysis. Impact may be incomplete and broader verification is recommended."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};

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

    #[test]
    fn explicit_paths_are_normalized_deduped_and_ordered() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("b.rs"), "fn b() {}\n").expect("write");
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").expect("write");
        let map = RepoMap {
            symbols: vec![
                sym("a", dir.path().join("a.rs").to_str().unwrap()),
                sym("b", dir.path().join("b.rs").to_str().unwrap()),
            ],
            relations: vec![],
        };
        let collected = collect_explicit_changes(
            dir.path(),
            &[
                "b.rs".to_string(),
                "a.rs".to_string(),
                "b.rs".to_string(),
                "./a.rs".to_string(),
            ],
            Some(&map),
        );
        assert_eq!(collected.files.len(), 2);
        assert_eq!(collected.files[0].path, "a.rs");
        assert_eq!(collected.files[1].path, "b.rs");
        assert!(collected.warnings.is_empty());
    }

    #[test]
    fn explicit_outside_root_is_excluded_with_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").expect("write");
        let collected = collect_explicit_changes(
            dir.path(),
            &["a.rs".to_string(), "../escape.rs".to_string()],
            None,
        );
        assert_eq!(collected.files.len(), 1);
        assert!(collected.warnings.iter().any(|w| w.contains("outside")));
    }

    #[test]
    fn explicit_absolute_inside_root_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let abs = dir.path().join("a.rs");
        std::fs::write(&abs, "fn a() {}\n").expect("write");
        let collected =
            collect_explicit_changes(dir.path(), &[abs.to_string_lossy().to_string()], None);
        assert_eq!(
            collected.files,
            vec![ChangedFile {
                path: "a.rs".to_string(),
                change_kind: ChangeKind::Modified,
            }]
        );
    }

    #[test]
    fn explicit_symlink_escape_is_excluded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::create_dir_all(&outside).expect("mkdir");
        std::fs::write(outside.join("secret.rs"), "fn s() {}\n").expect("write");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, project.join("link")).expect("symlink");
        #[cfg(unix)]
        {
            let collected =
                collect_explicit_changes(&project, &["link/secret.rs".to_string()], None);
            assert!(collected.files.is_empty());
            assert!(collected.warnings.iter().any(|w| w.contains("outside")));
        }
    }

    #[test]
    fn explicit_deleted_file_is_kept_as_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let collected = collect_explicit_changes(dir.path(), &["gone.rs".to_string()], None);
        assert_eq!(collected.files.len(), 1);
        assert_eq!(collected.files[0].change_kind, ChangeKind::Deleted);
    }

    #[test]
    fn explicit_missing_repomap_entry_is_added() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("new.rs"), "fn n() {}\n").expect("write");
        let map = RepoMap {
            symbols: vec![],
            relations: vec![],
        };
        let collected = collect_explicit_changes(dir.path(), &["new.rs".to_string()], Some(&map));
        assert_eq!(collected.files[0].change_kind, ChangeKind::Added);
    }

    #[test]
    fn active_collection_excludes_reverted_and_diverged() {
        use crate::provenance::store::ProvenanceStore;
        use crate::provenance::types::{
            ChangeCommittedEvent, ChangeKind as PKind, ChangeTarget, FileStateEvidence,
            ProvenanceEvent, file_content_hash,
        };
        let proj = tempfile::tempdir().expect("tempdir");
        std::fs::write(proj.path().join("a.txt"), "h1\n").expect("write");
        std::fs::write(proj.path().join("b.txt"), "k1\n").expect("write");
        let sess = tempfile::tempdir().expect("tempdir");
        let store = ProvenanceStore::new(sess.path().join("s"));
        // Active change for a.txt: h0 -> h1.
        let before = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h0\n")),
            byte_len: Some(3),
        };
        let after = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h1\n")),
            byte_len: Some(3),
        };
        let loaded = store.load_all().expect("load");
        let pred = ProvenanceStore::find_predecessor(&loaded.events, "a.txt", &before);
        let active = store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: None,
                    plan_item_id: None,
                    requirement_ids: vec![],
                    change_kind: PKind::TextEdit,
                    file: "a.txt".to_string(),
                    target: ChangeTarget::File,
                    before,
                    after,
                    predecessor_change_id: pred,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 1,
                }),
            )
            .expect("append");
        // Diverged change for b.txt: recorded h0->hx but workspace is k1.
        let b_before = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h0\n")),
            byte_len: Some(3),
        };
        let b_after = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("hx\n")),
            byte_len: Some(3),
        };
        let loaded = store.load_all().expect("load");
        let b_pred = ProvenanceStore::find_predecessor(&loaded.events, "b.txt", &b_before);
        store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: None,
                    plan_item_id: None,
                    requirement_ids: vec![],
                    change_kind: PKind::TextEdit,
                    file: "b.txt".to_string(),
                    target: ChangeTarget::File,
                    before: b_before,
                    after: b_after,
                    predecessor_change_id: b_pred,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 1,
                }),
            )
            .expect("append");
        let loaded = store.load_all().expect("load");
        let collected = collect_active_changes(proj.path(), &loaded.events, &[], None);
        assert!(collected.files.iter().any(|f| f.path == "a.txt"));
        assert!(!collected.files.iter().any(|f| f.path == "b.txt"));
        let _ = active;
    }

    #[test]
    fn identify_marks_all_file_symbols_as_file_level() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.rs");
        std::fs::write(&a, "fn a() {}\nfn b() {}\n").expect("write");
        let map = RepoMap {
            symbols: vec![sym("b", a.to_str().unwrap()), sym("a", a.to_str().unwrap())],
            relations: vec![],
        };
        let index = SymbolIdentityIndex::build(&map, dir.path()).expect("index");
        let changed = vec![ChangedFile {
            path: "a.rs".to_string(),
            change_kind: ChangeKind::Modified,
        }];
        let (symbols, _) = identify_changed_symbols(dir.path(), Some(&map), Some(&index), &changed);
        assert_eq!(symbols.len(), 2);
        assert!(
            symbols
                .iter()
                .all(|s| s.identification_confidence == IdentificationConfidence::FileLevel)
        );
        assert!(symbols.iter().all(|s| s.symbol_id.is_some()));
    }

    #[test]
    fn identify_deleted_file_is_unresolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let map = RepoMap {
            symbols: vec![],
            relations: vec![],
        };
        let changed = vec![ChangedFile {
            path: "gone.rs".to_string(),
            change_kind: ChangeKind::Deleted,
        }];
        let (symbols, warnings) = identify_changed_symbols(dir.path(), Some(&map), None, &changed);
        assert_eq!(symbols.len(), 1);
        assert_eq!(
            symbols[0].identification_confidence,
            IdentificationConfidence::Unresolved
        );
        assert!(warnings.iter().any(|w| w.contains("Deleted")));
    }

    #[test]
    fn identify_without_repomap_is_unresolved_with_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let changed = vec![ChangedFile {
            path: "a.rs".to_string(),
            change_kind: ChangeKind::Modified,
        }];
        let (symbols, warnings) = identify_changed_symbols(dir.path(), None, None, &changed);
        assert_eq!(
            symbols[0].identification_confidence,
            IdentificationConfidence::Unresolved
        );
        assert!(warnings.iter().any(|w| w.contains("RepoMap")));
    }

    #[test]
    fn stale_repomap_is_reported_with_broader_verification_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Workspace has `new_fn`, but the cached map still lists `old_fn`.
        std::fs::write(dir.path().join("stale.rs"), "fn new_fn() {}\n").expect("write");
        let map = RepoMap {
            symbols: vec![sym("old_fn", dir.path().join("stale.rs").to_str().unwrap())],
            relations: vec![],
        };
        let changed = vec![ChangedFile {
            path: "stale.rs".to_string(),
            change_kind: ChangeKind::Modified,
        }];
        let (_, warnings) = identify_changed_symbols(dir.path(), Some(&map), None, &changed);
        assert!(warnings.iter().any(|w| w.contains("stale")));
        assert!(warnings.iter().any(|w| w.contains("broader verification")));
    }

    #[test]
    fn active_collection_excludes_reverted_changes() {
        use crate::provenance::store::ProvenanceStore;
        use crate::provenance::types::{
            ChangeCommittedEvent, ChangeKind as PKind, ChangeTarget, FileStateEvidence,
            ProvenanceEvent, file_content_hash,
        };
        let proj = tempfile::tempdir().expect("tempdir");
        std::fs::write(proj.path().join("a.txt"), "h0\n").expect("write");
        let sess = tempfile::tempdir().expect("tempdir");
        let store = ProvenanceStore::new(sess.path().join("s"));
        // Original change h0 -> h1, then undo h1 -> h0 reverting it.
        std::fs::write(proj.path().join("a.txt"), "h1\n").expect("write");
        let before = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h0\n")),
            byte_len: Some(3),
        };
        let after = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h1\n")),
            byte_len: Some(3),
        };
        let loaded = store.load_all().expect("load");
        let pred = ProvenanceStore::find_predecessor(&loaded.events, "a.txt", &before);
        let original = store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: None,
                    plan_item_id: None,
                    requirement_ids: vec![],
                    change_kind: PKind::TextEdit,
                    file: "a.txt".to_string(),
                    target: ChangeTarget::File,
                    before,
                    after,
                    predecessor_change_id: pred,
                    reverts_change_id: None,
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 1,
                    lines_removed: 1,
                }),
            )
            .expect("append");
        std::fs::write(proj.path().join("a.txt"), "h0\n").expect("write");
        let undo_before = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h1\n")),
            byte_len: Some(3),
        };
        let undo_after = FileStateEvidence {
            exists: true,
            content_hash: Some(file_content_hash("h0\n")),
            byte_len: Some(3),
        };
        let loaded = store.load_all().expect("load");
        let undo_pred = ProvenanceStore::find_predecessor(&loaded.events, "a.txt", &undo_before);
        store
            .append(
                "s",
                ProvenanceEvent::ChangeCommitted(ChangeCommittedEvent {
                    transaction_id: String::new(),
                    directive_id: None,
                    plan_item_id: None,
                    requirement_ids: vec![],
                    change_kind: PKind::Undo,
                    file: "a.txt".to_string(),
                    target: ChangeTarget::File,
                    before: undo_before,
                    after: undo_after,
                    predecessor_change_id: undo_pred,
                    reverts_change_id: Some(original.event_id.clone()),
                    diff: "d".to_string(),
                    diff_hash: "blake3:x".to_string(),
                    lines_added: 0,
                    lines_removed: 0,
                }),
            )
            .expect("append");
        let loaded = store.load_all().expect("load");
        let collected = collect_active_changes(proj.path(), &loaded.events, &[], None);
        // The original is reverted: it must not appear as an active file.
        // The undo itself is active (bookkeeping), so the file may still be
        // listed, but the reverted change id must not be treated as active.
        let resolved = crate::provenance::query::resolve_active_states(proj.path(), &loaded.events);
        assert!(resolved.iter().any(|r| r.change_id == original.event_id
            && r.state == crate::provenance::query::ActiveChangeState::Reverted));
        let _ = collected;
    }
}
