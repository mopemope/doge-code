use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::analysis::parser::{AnalyzedSource, analyze_source};
use crate::analysis::{
    ContentFingerprint, RepoMap, SymbolId, SymbolIdentityIndex, SymbolInfo, SymbolKind,
    fingerprint_symbol, is_targetable_kind, normalize_relative_path, source_span, symbol_source,
};

/// Typed errors for transactional semantic edits.
///
/// String comparison must never be used to distinguish these cases.
#[derive(Debug, Error)]
pub enum SemanticEditError {
    #[error("unsupported symbol kind for semantic edit v1: {0}")]
    UnsupportedSymbolKind(String),
    #[error("target symbol no longer resolvable; re-run /edit-symbol")]
    TargetNotFound,
    #[error("invalid source span: {0}")]
    InvalidSourceSpan(String),
    #[error("target changed while the edit was being prepared; re-run /edit-symbol")]
    StaleTarget,
    #[error("file changed during commit; refusing to overwrite")]
    ConcurrentModification,
    #[error("invalid replacement: {0}")]
    InvalidReplacement(String),
    #[error("replacement changes symbol identity (rename/move not supported in v1)")]
    IdentityChanged,
    #[error("replacement introduces a syntax error")]
    IntroducedSyntaxError,
    #[error("failed to write file: {0}")]
    WriteFailed(String),
    #[error("analysis failed: {0}")]
    AnalysisFailed(String),
}

/// Prepared transaction state captured before the LLM call.
#[derive(Debug, Clone)]
pub struct PreparedSemanticEdit {
    pub symbol_id: SymbolId,
    pub expected_fingerprint: ContentFingerprint,
    pub file: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    pub parent: Option<String>,
    pub original_source: String,
    pub had_syntax_error: bool,
    /// 1-based line range at prepare time (for display/debugging; never used
    /// for apply-time positioning, which re-resolves via `SymbolId`).
    pub start_line: usize,
    pub end_line: usize,
}

/// Committed transaction result.
#[derive(Debug, Clone)]
pub struct SemanticEditResult {
    pub symbol_id: SymbolId,
    pub file: PathBuf,
    pub before_fingerprint: ContentFingerprint,
    pub after_fingerprint: ContentFingerprint,
    pub diff: String,
    pub lines_added: usize,
    pub lines_removed: usize,
}

fn resolve_absolute(project_root: &Path, file: &Path) -> PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        project_root.join(file)
    }
}

fn ensure_within_root(project_root: &Path, absolute: &Path) -> Result<PathBuf, SemanticEditError> {
    // Canonical comparison only: the previous lexical `||` fallback accepted
    // `/root/a/../../etc` lexically. For missing files, canonicalize the
    // parent directory instead so symlinked roots still match.
    let root_canon = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let file_canon = if let Ok(canon) = absolute.canonicalize() {
        canon
    } else if let Some(parent) = absolute.parent() {
        match parent.canonicalize() {
            Ok(canon_parent) => {
                if let Some(name) = absolute.file_name() {
                    canon_parent.join(name)
                } else {
                    absolute.to_path_buf()
                }
            }
            Err(_) => absolute.to_path_buf(),
        }
    } else {
        absolute.to_path_buf()
    };
    if file_canon.starts_with(&root_canon) {
        return Ok(absolute.to_path_buf());
    }
    Err(SemanticEditError::AnalysisFailed(format!(
        "file {} is outside project root {}",
        absolute.display(),
        project_root.display()
    )))
}

fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    // Canonicalized comparison tolerates symlinked temp dirs in tests.
    if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
        return ca == cb;
    }
    false
}

fn find_enclosing<'a>(symbols: &'a [SymbolInfo], file: &Path, line: u32) -> Option<&'a SymbolInfo> {
    let line = line as usize;
    let mut best: Option<&'a SymbolInfo> = None;
    let mut best_size = usize::MAX;
    for symbol in symbols.iter().filter(|s| same_file(&s.file, file)) {
        if symbol.start_line == 0 || symbol.end_line == 0 {
            continue;
        }
        if !(symbol.start_line <= line && line <= symbol.end_line) {
            continue;
        }
        let size = symbol.end_line.saturating_sub(symbol.start_line);
        if size < best_size {
            best_size = size;
            best = Some(symbol);
        }
    }
    best
}

/// Prepare from an already-read source snapshot (pure, testable).
pub fn prepare_from_source(
    project_root: &Path,
    absolute_file: &Path,
    line: u32,
    source: &str,
) -> Result<PreparedSemanticEdit, SemanticEditError> {
    let absolute = ensure_within_root(project_root, absolute_file)?;
    // Validate the path relativizes (fail closed on outside/escape).
    normalize_relative_path(project_root, &absolute)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;

    let analyzed: Option<AnalyzedSource> = analyze_source(&absolute, source)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    let Some(analyzed) = analyzed else {
        return Err(SemanticEditError::AnalysisFailed(format!(
            "no parser for {}",
            absolute.display()
        )));
    };

    let target = find_enclosing(&analyzed.repomap.symbols, &absolute, line)
        .ok_or(SemanticEditError::TargetNotFound)?;
    if !is_targetable_kind(&target.kind) {
        return Err(SemanticEditError::UnsupportedSymbolKind(
            target.kind.as_str().to_string(),
        ));
    }

    let index = SymbolIdentityIndex::build(&analyzed.repomap, project_root)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    let symbol_id = index
        .id_for_symbol(&analyzed.repomap, target)
        .cloned()
        .ok_or(SemanticEditError::TargetNotFound)?;

    let slice = symbol_source(source, target)
        .map_err(|e| SemanticEditError::InvalidSourceSpan(e.to_string()))?;
    let fingerprint = fingerprint_symbol(source, target)
        .map_err(|e| SemanticEditError::InvalidSourceSpan(e.to_string()))?;

    tracing::info!(
        symbol_id = %symbol_id,
        file = %absolute.display(),
        kind = target.kind.as_str(),
        "semantic_edit.prepare"
    );

    Ok(PreparedSemanticEdit {
        symbol_id,
        expected_fingerprint: fingerprint,
        file: absolute,
        name: target.name.clone(),
        kind: target.kind,
        parent: target.parent.clone(),
        original_source: slice.to_string(),
        had_syntax_error: analyzed.has_syntax_error,
        start_line: target.start_line,
        end_line: target.end_line,
    })
}

/// Synchronous file-backed prepare (tests and CLI paths).
pub fn prepare_edit(
    project_root: &Path,
    file: &Path,
    line: u32,
) -> Result<PreparedSemanticEdit, SemanticEditError> {
    let absolute = resolve_absolute(project_root, file);
    let source = std::fs::read_to_string(&absolute)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    prepare_from_source(project_root, &absolute, line, &source)
}

/// Async file-backed prepare (TUI job path).
pub async fn prepare_edit_async(
    project_root: &Path,
    file: &Path,
    line: u32,
) -> Result<PreparedSemanticEdit, SemanticEditError> {
    let absolute = resolve_absolute(project_root, file);
    let source = tokio::fs::read_to_string(&absolute)
        .await
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    prepare_from_source(project_root, &absolute, line, &source)
}

/// Build the candidate source in memory (pure).
pub fn build_candidate(
    current_source: &str,
    span: &crate::analysis::SourceSpan,
    replacement: &str,
) -> Result<String, SemanticEditError> {
    if replacement.trim().is_empty() {
        return Err(SemanticEditError::InvalidReplacement(
            "replacement is empty".to_string(),
        ));
    }
    if span.start_byte > span.end_byte
        || span.end_byte > current_source.len()
        || !current_source.is_char_boundary(span.start_byte)
        || !current_source.is_char_boundary(span.end_byte)
    {
        return Err(SemanticEditError::InvalidSourceSpan(
            "invalid current span".to_string(),
        ));
    }
    let mut candidate = String::with_capacity(current_source.len() + replacement.len());
    candidate.push_str(
        current_source
            .get(..span.start_byte)
            .ok_or_else(|| SemanticEditError::InvalidSourceSpan("bad start".to_string()))?,
    );
    candidate.push_str(replacement);
    // Preserve exactly one trailing newline state of the replacement as given;
    // do not normalize the rest of the file.
    candidate.push_str(
        current_source
            .get(span.end_byte..)
            .ok_or_else(|| SemanticEditError::InvalidSourceSpan("bad end".to_string()))?,
    );
    if candidate == current_source {
        return Err(SemanticEditError::InvalidReplacement(
            "replacement produced no changes".to_string(),
        ));
    }
    Ok(candidate)
}

fn count_diff_lines(diff: &str) -> (usize, usize) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

/// Pure apply over two snapshots.
///
/// `current_source` is the snapshot parsed for preconditions;
/// `pre_write_source` is the snapshot re-read immediately before write.
/// A mismatch yields `ConcurrentModification` without touching disk.
pub fn apply_with_snapshots(
    prepared: &PreparedSemanticEdit,
    replacement: &str,
    current_source: &str,
    pre_write_source: &str,
    project_root: &Path,
) -> Result<(String, SemanticEditResult, RepoMap), SemanticEditError> {
    if replacement.trim().is_empty() {
        return Err(SemanticEditError::InvalidReplacement(
            "replacement is empty".to_string(),
        ));
    }

    // Fresh parse of the current snapshot.
    let analyzed: Option<AnalyzedSource> = analyze_source(&prepared.file, current_source)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    let Some(analyzed) = analyzed else {
        return Err(SemanticEditError::AnalysisFailed(format!(
            "no parser for {}",
            prepared.file.display()
        )));
    };

    let index = SymbolIdentityIndex::build(&analyzed.repomap, project_root)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    let current_symbol = index
        .resolve(&analyzed.repomap, &prepared.symbol_id)
        .ok_or(SemanticEditError::TargetNotFound)?;

    // kind/parent/name must still match (rename/move guard; ID alone is opaque).
    if current_symbol.kind.as_str() != prepared.kind.as_str()
        || current_symbol.parent != prepared.parent
        || current_symbol.name != prepared.name
    {
        return Err(SemanticEditError::IdentityChanged);
    }

    let current_fp = fingerprint_symbol(current_source, current_symbol)
        .map_err(|e| SemanticEditError::InvalidSourceSpan(e.to_string()))?;
    if current_fp != prepared.expected_fingerprint {
        tracing::info!(
            symbol_id = %prepared.symbol_id,
            reason = "stale_target",
            "semantic_edit.precondition_failed"
        );
        return Err(SemanticEditError::StaleTarget);
    }

    let span = source_span(current_source, current_symbol)
        .map_err(|e| SemanticEditError::InvalidSourceSpan(e.to_string()))?;
    let candidate = build_candidate(current_source, &span, replacement)?;

    // Validate the candidate before touching disk.
    let candidate_analyzed: Option<AnalyzedSource> = analyze_source(&prepared.file, &candidate)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    let Some(candidate_analyzed) = candidate_analyzed else {
        return Err(SemanticEditError::AnalysisFailed(format!(
            "no parser for {}",
            prepared.file.display()
        )));
    };
    let candidate_index = SymbolIdentityIndex::build(&candidate_analyzed.repomap, project_root)
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    let candidate_symbol = candidate_index
        .resolve(&candidate_analyzed.repomap, &prepared.symbol_id)
        .ok_or(SemanticEditError::IdentityChanged)?;
    if candidate_symbol.kind.as_str() != prepared.kind.as_str()
        || candidate_symbol.parent != prepared.parent
        || candidate_symbol.name != prepared.name
    {
        return Err(SemanticEditError::IdentityChanged);
    }
    if !analyzed.has_syntax_error && candidate_analyzed.has_syntax_error {
        return Err(SemanticEditError::IntroducedSyntaxError);
    }
    let after_fp = fingerprint_symbol(&candidate, candidate_symbol)
        .map_err(|e| SemanticEditError::InvalidSourceSpan(e.to_string()))?;

    // Write-race check: the file must be unchanged since we parsed it.
    if pre_write_source != current_source {
        return Err(SemanticEditError::ConcurrentModification);
    }

    let diff = diffy::create_patch(current_source, &candidate).to_string();
    let (lines_added, lines_removed) = count_diff_lines(&diff);

    tracing::info!(
        symbol_id = %prepared.symbol_id,
        file = %prepared.file.display(),
        lines_added,
        lines_removed,
        "semantic_edit.commit"
    );

    let result = SemanticEditResult {
        symbol_id: prepared.symbol_id.clone(),
        file: prepared.file.clone(),
        before_fingerprint: current_fp,
        after_fingerprint: after_fp,
        diff,
        lines_added,
        lines_removed,
    };
    Ok((candidate, result, candidate_analyzed.repomap))
}

/// Atomic write via a sibling temp file (same directory) + persist.
pub fn atomic_write(path: &Path, content: &str) -> Result<(), SemanticEditError> {
    if content.as_bytes().contains(&0) {
        return Err(SemanticEditError::WriteFailed(
            "binary content is not allowed".to_string(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| SemanticEditError::WriteFailed("no parent directory".to_string()))?;
    std::fs::create_dir_all(parent).map_err(|e| SemanticEditError::WriteFailed(e.to_string()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".tmp_semantic_edit_")
        .tempfile_in(parent)
        .map_err(|e| SemanticEditError::WriteFailed(e.to_string()))?;
    use std::io::Write as _;
    temp.write_all(content.as_bytes())
        .map_err(|e| SemanticEditError::WriteFailed(e.to_string()))?;
    temp.persist(path)
        .map_err(|e| SemanticEditError::WriteFailed(e.to_string()))?;
    Ok(())
}

/// File-backed apply: re-reads, validates, race-checks, then atomically writes.
///
/// Returns the commit result plus the candidate file `RepoMap` for shared-map
/// updates. No session/undo side effects happen here; the caller commits them
/// only on `Ok`.
pub async fn apply_edit(
    prepared: &PreparedSemanticEdit,
    replacement: &str,
    project_root: &Path,
) -> Result<(SemanticEditResult, RepoMap, String), SemanticEditError> {
    let current_source = tokio::fs::read_to_string(&prepared.file)
        .await
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    // Validate against the current snapshot first (pure, no I/O).
    // Pre-write snapshot is re-read after validation to close the race.
    let (candidate, _result, _candidate_map) = apply_with_snapshots(
        prepared,
        replacement,
        &current_source,
        &current_source,
        project_root,
    )?;

    let pre_write_source = tokio::fs::read_to_string(&prepared.file)
        .await
        .map_err(|e| SemanticEditError::AnalysisFailed(e.to_string()))?;
    if pre_write_source != current_source {
        return Err(SemanticEditError::ConcurrentModification);
    }
    // Re-run the pure check with the real pre-write snapshot for safety.
    let (candidate2, result2, candidate_map2) = apply_with_snapshots(
        prepared,
        replacement,
        &current_source,
        &pre_write_source,
        project_root,
    )?;
    debug_assert_eq!(candidate, candidate2);
    atomic_write(&prepared.file, &candidate)?;
    Ok((result2, candidate_map2, current_source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_temp_project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, content) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(content.as_bytes()).unwrap();
        }
        dir
    }

    fn prepare_for(dir: &Path, rel: &str, line: u32) -> PreparedSemanticEdit {
        let file = dir.join(rel);
        prepare_edit(dir, &file, line).unwrap()
    }

    #[test]
    fn test_normal_success() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {\n    1;\n}\nfn bar() {}\n")]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 2);
        let current = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let (candidate, result, _) = apply_with_snapshots(
            &prepared,
            "fn foo() {\n    2;\n}\n",
            &current,
            &current,
            root,
        )
        .unwrap();
        assert!(candidate.contains("2;"));
        assert!(candidate.contains("fn bar() {}"));
        assert_ne!(result.before_fingerprint, result.after_fingerprint);
    }

    #[test]
    fn test_stale_target() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {\n    1;\n}\n")]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 2);
        // External change to the target body.
        let changed = "fn foo() {\n    99;\n}\n";
        let err =
            apply_with_snapshots(&prepared, "fn foo() {\n    2;\n}\n", changed, changed, root)
                .unwrap_err();
        assert!(matches!(err, SemanticEditError::StaleTarget));
    }

    #[test]
    fn test_line_shift_succeeds() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {\n    1;\n}\n")]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 1);
        // External insertion above the target; body unchanged.
        let shifted = "// new comment\nfn foo() {\n    1;\n}\n";
        let (candidate, _, _) =
            apply_with_snapshots(&prepared, "fn foo() {\n    2;\n}\n", shifted, shifted, root)
                .unwrap();
        assert!(candidate.starts_with("// new comment\n"));
        assert!(candidate.contains("2;"));
    }

    #[test]
    fn test_unrelated_same_file_edit_preserved() {
        let dir = write_temp_project(&[(
            "src/lib.rs",
            "fn foo() {\n    1;\n}\nfn other() {\n    1;\n}\n",
        )]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 2);
        let current = "fn foo() {\n    1;\n}\nfn other() {\n    99;\n}\n";
        let (candidate, _, _) =
            apply_with_snapshots(&prepared, "fn foo() {\n    2;\n}\n", current, current, root)
                .unwrap();
        assert!(candidate.contains("fn foo() {\n    2;\n}"));
        assert!(candidate.contains("99;"));
    }

    #[test]
    fn test_concurrent_modification() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {\n    1;\n}\n")]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 2);
        let current = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let pre_write = "fn foo() {\n    1;\n}\n// race\n";
        let err = apply_with_snapshots(
            &prepared,
            "fn foo() {\n    2;\n}\n",
            &current,
            pre_write,
            root,
        )
        .unwrap_err();
        assert!(matches!(err, SemanticEditError::ConcurrentModification));
    }

    #[test]
    fn test_rename_replacement_rejected() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {}\n")]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 1);
        let current = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let err =
            apply_with_snapshots(&prepared, "fn bar() {}\n", &current, &current, root).unwrap_err();
        assert!(matches!(err, SemanticEditError::IdentityChanged));
    }

    #[test]
    fn test_empty_replacement_rejected() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {}\n")]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 1);
        let current = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let err = apply_with_snapshots(&prepared, "   \n", &current, &current, root).unwrap_err();
        assert!(matches!(err, SemanticEditError::InvalidReplacement(_)));
    }

    #[test]
    fn test_syntax_regression_rejected() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {\n    1;\n}\n")]);
        let root = dir.path();
        let prepared = prepare_for(root, "src/lib.rs", 2);
        let current = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let err = apply_with_snapshots(&prepared, "fn foo() {\n    1;\n", &current, &current, root)
            .unwrap_err();
        // Candidate missing closing brace: either identity loss or syntax error.
        assert!(matches!(
            err,
            SemanticEditError::IntroducedSyntaxError | SemanticEditError::IdentityChanged
        ));
    }

    #[test]
    fn test_unsupported_kind() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {\n    let x = 1;\n}\n")]);
        let root = dir.path();
        // Find a Variable symbol line: `let x` is on line 2 but enclosing is fn;
        // force a variable-only file instead.
        let dir2 = write_temp_project(&[("src/v.rs", "fn f() {\n    let myvar = 1;\n}\n")]);
        let root2 = dir2.path();
        // Prepare directly from a variable symbol to hit the guard.
        let source = std::fs::read_to_string(root2.join("src/v.rs")).unwrap();
        let analyzed = analyze_source(&root2.join("src/v.rs"), &source)
            .unwrap()
            .unwrap();
        let var = analyzed
            .repomap
            .symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Variable)
            .expect("variable symbol");
        // prepare_from_source picks the innermost enclosing symbol (fn), so
        // exercise the guard via a synthetic single-symbol map instead.
        let _ = (root, var);
        let single = RepoMap {
            symbols: vec![var.clone()],
            relations: vec![],
        };
        assert!(!is_targetable_kind(&single.symbols[0].kind));
    }

    #[tokio::test]
    async fn test_file_apply_writes_atomically() {
        let dir = write_temp_project(&[("src/lib.rs", "fn foo() {\n    1;\n}\n")]);
        let root = dir.path().to_path_buf();
        let prepared = prepare_edit(&root, &root.join("src/lib.rs"), 2).unwrap();
        let (result, _, before) = apply_edit(&prepared, "fn foo() {\n    2;\n}\n", &root)
            .await
            .unwrap();
        let after = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(after.contains("2;"));
        assert!(before.contains("1;"));
        assert!(!result.diff.is_empty());
    }
}
