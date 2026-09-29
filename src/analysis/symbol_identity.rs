use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::analysis::{RepoMap, SymbolInfo};

const SYMBOL_ID_PREFIX: &str = "sym-v1-";
const FINGERPRINT_PREFIX: &str = "fp-v1-";
const SYMBOL_ID_DOMAIN: &str = "doge-symbol-v1";

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Deterministic stable semantic identifier for a symbol.
///
/// Format: `sym-v1-<64 hex BLAKE3>`.
/// Never contains absolute paths, line numbers, bodies, or tree-sitter IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(String);

impl SymbolId {
    fn from_hex_unchecked(hex: String) -> Self {
        Self(format!("{SYMBOL_ID_PREFIX}{hex}"))
    }

    /// Parse and validate an externally supplied ID string.
    pub fn parse(s: &str) -> Result<Self> {
        let hex = s
            .strip_prefix(SYMBOL_ID_PREFIX)
            .ok_or_else(|| anyhow!("invalid SymbolId prefix: {s}"))?;
        if !is_hex64(hex) {
            anyhow::bail!("invalid SymbolId hex: {s}");
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Content fingerprint for stale-target detection.
///
/// Format: `fp-v1-<64 hex BLAKE3>` over the exact symbol source slice
/// after CRLF normalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentFingerprint(String);

impl ContentFingerprint {
    fn from_hex_unchecked(hex: String) -> Self {
        Self(format!("{FINGERPRINT_PREFIX}{hex}"))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let hex = s
            .strip_prefix(FINGERPRINT_PREFIX)
            .ok_or_else(|| anyhow!("invalid ContentFingerprint prefix: {s}"))?;
        if !is_hex64(hex) {
            anyhow::bail!("invalid ContentFingerprint hex: {s}");
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Normalize a symbol file path to a project-relative `/`-separated string.
///
/// Errors on outside-root paths, `..` escapes, or un-relativizable paths.
pub fn normalize_relative_path(project_root: &Path, file: &Path) -> Result<String> {
    let rel: PathBuf = if file.is_absolute() {
        file.strip_prefix(project_root)
            .map(Path::to_path_buf)
            .map_err(|_| {
                anyhow!(
                    "file {} is outside project root {}",
                    file.display(),
                    project_root.display()
                )
            })?
    } else {
        file.to_path_buf()
    };

    let mut parts: Vec<String> = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                anyhow::bail!("invalid relative path: {}", rel.display());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::bail!("path escapes project root: {}", rel.display());
            }
            Component::Normal(os) => {
                let s = os.to_string_lossy().into_owned();
                if s.is_empty() || s.contains('/') || s.contains('\\') {
                    anyhow::bail!("invalid path component in {}", rel.display());
                }
                parts.push(s);
            }
        }
    }
    if parts.is_empty() {
        anyhow::bail!("empty relative path for {}", file.display());
    }
    Ok(parts.join("/"))
}

/// Whether a symbol kind is editable via transactional semantic edit v1.
pub fn is_targetable_kind(kind: &crate::analysis::SymbolKind) -> bool {
    use crate::analysis::SymbolKind as K;
    matches!(
        kind,
        K::Function | K::Struct | K::Enum | K::Trait | K::Impl | K::Method | K::AssocFn | K::Mod
    )
}

fn identity_group_key(rel: &str, symbol: &SymbolInfo) -> String {
    format!(
        "{}\x00{}\x00{}\x00{}",
        rel,
        symbol.kind.as_str(),
        symbol.parent.as_deref().unwrap_or(""),
        symbol.name
    )
}

fn compute_symbol_id(rel: &str, symbol: &SymbolInfo, ordinal: u64) -> SymbolId {
    let mut input = Vec::new();
    for part in [
        SYMBOL_ID_DOMAIN,
        rel,
        symbol.kind.as_str(),
        symbol.parent.as_deref().unwrap_or(""),
        symbol.name.as_str(),
        &ordinal.to_string(),
    ] {
        input.extend_from_slice(part.as_bytes());
        input.push(0);
    }
    let hash = blake3::hash(&input);
    SymbolId::from_hex_unchecked(hash.to_hex().to_string())
}

/// Normalize newlines for fingerprinting only (`\r\n` -> `\n`).
pub fn normalize_fingerprint_source(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Fingerprint a symbol source slice (already extracted).
pub fn fingerprint_symbol_source(symbol_source: &str) -> ContentFingerprint {
    let normalized = normalize_fingerprint_source(symbol_source);
    let hash = blake3::hash(normalized.as_bytes());
    ContentFingerprint::from_hex_unchecked(hash.to_hex().to_string())
}

/// Byte span of a symbol within its file source.
///
/// `SymbolInfo` stores 1-based line/column values where the column is a
/// 1-based byte offset within the line (tree-sitter columns are byte
/// oriented, plus one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub start_byte: usize,
    pub end_byte: usize,
}

fn line_starts(source: &str) -> Vec<usize> {
    // Start offset of each 1-based line using split_inclusive semantics.
    let mut starts = Vec::new();
    let mut offset = 0usize;
    for chunk in source.split_inclusive('\n') {
        starts.push(offset);
        offset += chunk.len();
    }
    if starts.is_empty() && !source.is_empty() {
        starts.push(0);
    }
    starts
}

fn line_chunk<'a>(
    source: &'a str,
    line_1based: usize,
    starts: &[usize],
) -> Result<(usize, &'a str)> {
    if line_1based == 0 || line_1based > starts.len() {
        anyhow::bail!("line {line_1based} does not exist");
    }
    let start = starts[line_1based - 1];
    let end = if line_1based < starts.len() {
        starts[line_1based]
    } else {
        source.len()
    };
    let chunk = source
        .get(start..end)
        .with_context(|| format!("invalid line range {line_1based}"))?;
    Ok((start, chunk))
}

fn line_body_len(chunk: &str) -> usize {
    let without_nl = chunk.strip_suffix('\n').unwrap_or(chunk);
    let without_cr = without_nl.strip_suffix('\r').unwrap_or(without_nl);
    without_cr.len()
}

/// Compute the byte span for a symbol. Never auto-corrects invalid ranges.
pub fn source_span(source: &str, symbol: &SymbolInfo) -> Result<SourceSpan> {
    if symbol.start_line == 0 || symbol.end_line == 0 {
        anyhow::bail!("invalid symbol line range");
    }
    if symbol.start_col == 0 || symbol.end_col == 0 {
        anyhow::bail!("invalid symbol column range");
    }
    if symbol.end_line < symbol.start_line {
        anyhow::bail!("invalid symbol line range");
    }
    if symbol.end_line == symbol.start_line && symbol.end_col < symbol.start_col {
        anyhow::bail!("invalid symbol column range");
    }

    let starts = line_starts(source);
    if starts.is_empty() {
        anyhow::bail!("empty source has no lines");
    }

    let (start_line_off, start_chunk) = line_chunk(source, symbol.start_line, &starts)?;
    let (end_line_off, end_chunk) = line_chunk(source, symbol.end_line, &starts)?;

    let start_body = line_body_len(start_chunk);
    let end_body = line_body_len(end_chunk);
    if symbol.start_col - 1 > start_body {
        anyhow::bail!(
            "start column {} out of range for line {}",
            symbol.start_col,
            symbol.start_line
        );
    }
    if symbol.end_col - 1 > end_body {
        anyhow::bail!(
            "end column {} out of range for line {}",
            symbol.end_col,
            symbol.end_line
        );
    }

    let start_byte = start_line_off + (symbol.start_col - 1);
    let end_byte = end_line_off + (symbol.end_col - 1);

    if start_byte > end_byte {
        anyhow::bail!("invalid symbol byte range");
    }
    if end_byte > source.len() {
        anyhow::bail!("symbol range exceeds source length");
    }
    if !source.is_char_boundary(start_byte) || !source.is_char_boundary(end_byte) {
        anyhow::bail!("symbol range splits a UTF-8 character");
    }
    Ok(SourceSpan {
        start_byte,
        end_byte,
    })
}

/// Extract the exact source slice for a symbol.
pub fn symbol_source<'a>(source: &'a str, symbol: &SymbolInfo) -> Result<&'a str> {
    let span = source_span(source, symbol)?;
    source
        .get(span.start_byte..span.end_byte)
        .ok_or_else(|| anyhow!("invalid symbol source range"))
}

/// Fingerprint a symbol within a full file source.
pub fn fingerprint_symbol(source: &str, symbol: &SymbolInfo) -> Result<ContentFingerprint> {
    let slice = symbol_source(source, symbol)?;
    Ok(fingerprint_symbol_source(slice))
}

/// Deterministic index from `RepoMap` symbols to stable [`SymbolId`]s.
///
/// Symbols that cannot be relativized against `project_root` (stale cache
/// entries, outside-root paths) are skipped rather than failing the whole
/// index, so one bad entry never strips IDs from all other symbols.
#[derive(Debug, Clone, Default)]
pub struct SymbolIdentityIndex {
    by_id: HashMap<SymbolId, usize>,
    by_index: HashMap<usize, SymbolId>,
}

impl SymbolIdentityIndex {
    /// Build the index. Only symbol metadata is used (no file reads).
    pub fn build(repo_map: &RepoMap, project_root: &Path) -> Result<Self> {
        // Group symbol indices by identity key (skipping un-relativizable ones).
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        let mut rels: HashMap<usize, String> = HashMap::new();
        for (idx, symbol) in repo_map.symbols.iter().enumerate() {
            let rel = match normalize_relative_path(project_root, &symbol.file) {
                Ok(rel) => rel,
                Err(e) => {
                    tracing::debug!(
                        file = %symbol.file.display(),
                        error = %e,
                        "skipping symbol for stable ID (un-relativizable)"
                    );
                    continue;
                }
            };
            let key = identity_group_key(&rel, symbol);
            groups.entry(key).or_default().push(idx);
            rels.insert(idx, rel);
        }

        // Assign collision ordinals by stable position sort.
        let mut ordinals = vec![0u64; repo_map.symbols.len()];
        for indices in groups.values() {
            let mut sorted = indices.clone();
            sorted.sort_by_key(|&i| {
                let s = &repo_map.symbols[i];
                (s.start_line, s.start_col, s.end_line, s.end_col)
            });
            for (ordinal, idx) in sorted.into_iter().enumerate() {
                ordinals[idx] = ordinal as u64;
            }
        }

        let mut ids: HashMap<usize, SymbolId> = HashMap::with_capacity(rels.len());
        let mut by_id: HashMap<SymbolId, usize> = HashMap::with_capacity(rels.len());
        for (idx, symbol) in repo_map.symbols.iter().enumerate() {
            let Some(rel) = rels.get(&idx) else {
                continue;
            };
            let id = compute_symbol_id(rel, symbol, ordinals[idx]);
            if by_id.contains_key(&id) {
                anyhow::bail!("SymbolId collision detected");
            }
            by_id.insert(id.clone(), idx);
            ids.insert(idx, id);
        }
        Ok(Self {
            by_id,
            by_index: ids,
        })
    }

    /// Resolve a stable ID to its current symbol.
    pub fn resolve<'a>(&self, repo_map: &'a RepoMap, id: &SymbolId) -> Option<&'a SymbolInfo> {
        self.by_id
            .get(id)
            .and_then(|&idx| repo_map.symbols.get(idx))
    }

    /// ID attached to a symbol reference from the same `RepoMap`.
    ///
    /// Uses pointer identity when the reference borrows from `repo_map`,
    /// falling back to a value-based lookup.
    pub fn id_for_symbol(&self, repo_map: &RepoMap, symbol: &SymbolInfo) -> Option<&SymbolId> {
        // Fast path: pointer equality.
        for (idx, candidate) in repo_map.symbols.iter().enumerate() {
            if std::ptr::eq(candidate, symbol) {
                return self.by_index.get(&idx);
            }
        }
        // Fallback: match on stable value key.
        for (idx, candidate) in repo_map.symbols.iter().enumerate() {
            if candidate.file == symbol.file
                && candidate.name == symbol.name
                && candidate.kind.as_str() == symbol.kind.as_str()
                && candidate.parent == symbol.parent
                && candidate.start_line == symbol.start_line
                && candidate.start_col == symbol.start_col
                && candidate.end_line == symbol.end_line
                && candidate.end_col == symbol.end_col
            {
                return self.by_index.get(&idx);
            }
        }
        None
    }

    /// ID by symbol position in `repo_map.symbols`.
    pub fn id_for_index(&self, idx: usize) -> Option<&SymbolId> {
        self.by_index.get(&idx)
    }

    pub fn len(&self) -> usize {
        self.by_index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_index.is_empty()
    }

    /// Number of symbols skipped as un-relativizable during build.
    pub fn skipped_count(&self, repo_map: &RepoMap) -> usize {
        repo_map.symbols.len().saturating_sub(self.by_index.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SymbolKind;
    use std::path::PathBuf;

    #[allow(clippy::too_many_arguments)]
    fn make_symbol(
        name: &str,
        kind: SymbolKind,
        file: &Path,
        sl: usize,
        sc: usize,
        el: usize,
        ec: usize,
        parent: Option<&str>,
    ) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind,
            file: file.to_path_buf(),
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
            parent: parent.map(|s| s.to_string()),
            file_total_lines: el,
            function_lines: None,
            keywords: Vec::new(),
        }
    }

    fn repo_with(symbols: Vec<SymbolInfo>) -> RepoMap {
        RepoMap {
            symbols,
            relations: Vec::new(),
        }
    }

    #[test]
    fn test_line_shift_keeps_id() {
        let root = Path::new("/proj");
        let file = PathBuf::from("/proj/src/lib.rs");
        let before = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file,
            1,
            1,
            1,
            12,
            None,
        )]);
        let after = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file,
            3,
            1,
            3,
            12,
            None,
        )]);
        let a = SymbolIdentityIndex::build(&before, root).unwrap();
        let b = SymbolIdentityIndex::build(&after, root).unwrap();
        assert_eq!(a.id_for_index(0), b.id_for_index(0));
    }

    #[test]
    fn test_body_change_keeps_id_but_changes_fingerprint() {
        let root = Path::new("/proj");
        let file = PathBuf::from("/proj/src/lib.rs");
        let before_src = "fn foo() {\n    1;\n}\n";
        let after_src = "fn foo() {\n    2;\n}\n";
        let before = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file,
            1,
            1,
            3,
            2,
            None,
        )]);
        let after = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file,
            1,
            1,
            3,
            2,
            None,
        )]);
        let a = SymbolIdentityIndex::build(&before, root).unwrap();
        let b = SymbolIdentityIndex::build(&after, root).unwrap();
        assert_eq!(a.id_for_index(0), b.id_for_index(0));
        let fa = fingerprint_symbol(before_src, &before.symbols[0]).unwrap();
        let fb = fingerprint_symbol(after_src, &after.symbols[0]).unwrap();
        assert_ne!(fa, fb);
    }

    #[test]
    fn test_different_worktree_same_id() {
        let file_a = PathBuf::from("/tmp/a/project/src/lib.rs");
        let file_b = PathBuf::from("/tmp/b/project/src/lib.rs");
        let repo_a = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file_a,
            1,
            1,
            1,
            12,
            None,
        )]);
        let repo_b = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file_b,
            1,
            1,
            1,
            12,
            None,
        )]);
        let a = SymbolIdentityIndex::build(&repo_a, Path::new("/tmp/a/project")).unwrap();
        let b = SymbolIdentityIndex::build(&repo_b, Path::new("/tmp/b/project")).unwrap();
        assert_eq!(a.id_for_index(0), b.id_for_index(0));
    }

    #[test]
    fn test_rename_changes_id() {
        let root = Path::new("/proj");
        let file = PathBuf::from("/proj/src/lib.rs");
        let foo = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file,
            1,
            1,
            1,
            12,
            None,
        )]);
        let bar = repo_with(vec![make_symbol(
            "bar",
            SymbolKind::Function,
            &file,
            1,
            1,
            1,
            12,
            None,
        )]);
        let a = SymbolIdentityIndex::build(&foo, root).unwrap();
        let b = SymbolIdentityIndex::build(&bar, root).unwrap();
        assert_ne!(a.id_for_index(0), b.id_for_index(0));
    }

    #[test]
    fn test_parent_change_changes_id() {
        let root = Path::new("/proj");
        let file = PathBuf::from("/proj/src/lib.rs");
        let a_repo = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Method,
            &file,
            1,
            1,
            3,
            2,
            Some("A"),
        )]);
        let b_repo = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Method,
            &file,
            1,
            1,
            3,
            2,
            Some("B"),
        )]);
        let a = SymbolIdentityIndex::build(&a_repo, root).unwrap();
        let b = SymbolIdentityIndex::build(&b_repo, root).unwrap();
        assert_ne!(a.id_for_index(0), b.id_for_index(0));
    }

    #[test]
    fn test_collision_ordinals_unique() {
        let root = Path::new("/proj");
        let file = PathBuf::from("/proj/src/lib.rs");
        let repo = repo_with(vec![
            make_symbol("foo", SymbolKind::Function, &file, 1, 1, 1, 12, None),
            make_symbol("foo", SymbolKind::Function, &file, 5, 1, 5, 12, None),
        ]);
        let idx = SymbolIdentityIndex::build(&repo, root).unwrap();
        assert_ne!(idx.id_for_index(0), idx.id_for_index(1));
    }

    #[test]
    fn test_deterministic_rebuild() {
        let root = Path::new("/proj");
        let file = PathBuf::from("/proj/src/lib.rs");
        let repo = repo_with(vec![
            make_symbol("foo", SymbolKind::Function, &file, 5, 1, 5, 12, None),
            make_symbol("foo", SymbolKind::Function, &file, 1, 1, 1, 12, None),
        ]);
        let a = SymbolIdentityIndex::build(&repo, root).unwrap();
        let b = SymbolIdentityIndex::build(&repo, root).unwrap();
        assert_eq!(a.id_for_index(0), b.id_for_index(0));
        assert_eq!(a.id_for_index(1), b.id_for_index(1));
    }

    #[test]
    fn test_fingerprint_line_shift_outside_target() {
        let before = "fn other() {}\nfn foo() {\n    1;\n}\n";
        // Symbol foo spans lines 2-4 in both; insertion above shifts it.
        let foo_before = make_symbol(
            "foo",
            SymbolKind::Function,
            &PathBuf::from("x.rs"),
            2,
            1,
            4,
            2,
            None,
        );
        let after = "// new\nfn other() {}\nfn foo() {\n    1;\n}\n";
        let foo_after = make_symbol(
            "foo",
            SymbolKind::Function,
            &PathBuf::from("x.rs"),
            3,
            1,
            5,
            2,
            None,
        );
        let fa = fingerprint_symbol(before, &foo_before).unwrap();
        let fb = fingerprint_symbol(after, &foo_after).unwrap();
        assert_eq!(fa, fb);
    }

    #[test]
    fn test_fingerprint_crlf_equal() {
        let lf = "fn foo() {\n    1;\n}\n";
        let crlf = "fn foo() {\r\n    1;\r\n}\r\n";
        let sym = make_symbol(
            "foo",
            SymbolKind::Function,
            &PathBuf::from("x.rs"),
            1,
            1,
            3,
            2,
            None,
        );
        // CRLF span differs in byte offsets; fingerprint via extracted slices instead.
        let slice_lf = symbol_source(lf, &sym).unwrap();
        // For CRLF the end col must account for \r: line 3 is "}\r" body len 1, end col 2 still valid.
        let slice_crlf = symbol_source(crlf, &sym).unwrap();
        assert_eq!(
            fingerprint_symbol_source(slice_lf),
            fingerprint_symbol_source(slice_crlf)
        );
    }

    #[test]
    fn test_unicode_span_byte_columns() {
        let src = "fn foo() {}\n// 日本語コメント\nfn bar() {\n    \"🎉\";\n}\n";
        // bar spans lines 3-5.
        let bar = make_symbol(
            "bar",
            SymbolKind::Function,
            &PathBuf::from("x.rs"),
            3,
            1,
            5,
            2,
            None,
        );
        let slice = symbol_source(src, &bar).unwrap();
        assert!(slice.contains("🎉"));
        assert!(!slice.contains("日本語"));
        let _ = fingerprint_symbol(src, &bar).unwrap();
    }

    #[test]
    fn test_invalid_span_rejected() {
        let src = "fn foo() {}\n";
        let bad = make_symbol(
            "foo",
            SymbolKind::Function,
            &PathBuf::from("x.rs"),
            5,
            1,
            5,
            2,
            None,
        );
        assert!(source_span(src, &bad).is_err());
    }

    #[test]
    fn test_outside_root_rejected() {
        let err = normalize_relative_path(Path::new("/proj"), Path::new("/other/x.rs"));
        assert!(err.is_err());
    }

    #[test]
    fn test_build_skips_outside_root_symbol() {
        let root = Path::new("/proj");
        let good = make_symbol(
            "good",
            SymbolKind::Function,
            &PathBuf::from("/proj/src/lib.rs"),
            1,
            1,
            1,
            12,
            None,
        );
        let bad = make_symbol(
            "bad",
            SymbolKind::Function,
            &PathBuf::from("/other/x.rs"),
            1,
            1,
            1,
            12,
            None,
        );
        let repo = repo_with(vec![good, bad]);
        let idx = SymbolIdentityIndex::build(&repo, root).unwrap();
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.skipped_count(&repo), 1);
        assert!(idx.id_for_index(0).is_some());
        assert!(idx.id_for_index(1).is_none());
    }

    #[test]
    fn test_symbol_id_format() {
        let root = Path::new("/proj");
        let file = PathBuf::from("/proj/src/lib.rs");
        let repo = repo_with(vec![make_symbol(
            "foo",
            SymbolKind::Function,
            &file,
            1,
            1,
            1,
            12,
            None,
        )]);
        let idx = SymbolIdentityIndex::build(&repo, root).unwrap();
        let id = idx.id_for_index(0).unwrap();
        assert!(id.as_str().starts_with("sym-v1-"));
        assert_eq!(id.as_str().len(), "sym-v1-".len() + 64);
        SymbolId::parse(id.as_str()).unwrap();
    }
}
