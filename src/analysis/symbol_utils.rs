use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};

/// シンボルの開始・終了行情報を保持する軽量構造体。
#[derive(Debug, Clone)]
pub struct SymbolSpan {
    pub file: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based inclusive start line
    pub start_line: u32,
    /// 1-based inclusive end line
    pub end_line: u32,
    pub parent: Option<String>,
}

impl SymbolSpan {
    #[inline]
    pub fn contains_line(&self, line: u32) -> bool {
        self.start_line <= line && line <= self.end_line
    }

    #[inline]
    pub fn is_inner_of(&self, other: &SymbolSpan) -> bool {
        self.start_line >= other.start_line
            && self.end_line <= other.end_line
            && (self.start_line > other.start_line || self.end_line < other.end_line)
    }
}

fn symbol_info_to_span(info: &SymbolInfo) -> Option<SymbolSpan> {
    if info.start_line == 0 || info.end_line == 0 || info.end_line < info.start_line {
        return None;
    }

    Some(SymbolSpan {
        file: info.file.clone(),
        name: info.name.clone(),
        kind: info.kind,
        start_line: info.start_line as u32,
        end_line: info.end_line as u32,
        parent: info.parent.clone(),
    })
}

/// 指定ファイル内のシンボル一覧を返す。
///
/// RepoMap に含まれる行情報を利用し、行範囲が不正なシンボルは除外する。
pub fn list_symbols(repo_map: &RepoMap, file: &Path) -> Result<Vec<SymbolSpan>> {
    let file = file
        .canonicalize()
        .or_else(|_| Ok(file.to_path_buf()))
        .map_err(|e: io::Error| anyhow!("invalid file path: {e}"))?;

    let symbols = repo_map
        .symbols
        .iter()
        .filter(|s| s.file == file)
        .filter_map(symbol_info_to_span)
        .collect();

    Ok(symbols)
}

/// 指定ファイルと行番号を含む「もっとも内側のシンボル」を返す。
/// 行番号は 1-based を想定。
pub fn find_enclosing_symbol(
    repo_map: &RepoMap,
    file: &Path,
    line: u32,
) -> Result<Option<SymbolSpan>> {
    let candidates = list_symbols(repo_map, file)
        .with_context(|| format!("failed to list symbols for {}", file.display()))?;

    let mut best: Option<SymbolSpan> = None;

    for sym in candidates.into_iter().filter(|s| s.contains_line(line)) {
        match &best {
            None => best = Some(sym),
            Some(current) => {
                if sym.is_inner_of(current) {
                    best = Some(sym);
                }
            }
        }
    }

    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_symbol(
        name: &str,
        file: &Path,
        start: u32,
        end: u32,
        parent: Option<&str>,
        kind: SymbolKind,
    ) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind,
            file: file.to_path_buf(),
            start_line: start as usize,
            start_col: 0,
            end_line: end as usize,
            end_col: 0,
            parent: parent.map(|p| p.to_string()),
            file_total_lines: end as usize,
            function_lines: None,
            keywords: Vec::new(),
        }
    }

    #[test]
    fn symbol_span_contains_and_inner() {
        let file = PathBuf::from("src/example.rs");
        let outer = SymbolSpan {
            file: file.clone(),
            name: "outer".into(),
            kind: SymbolKind::Function,
            start_line: 5,
            end_line: 30,
            parent: None,
        };
        let inner = SymbolSpan {
            file,
            name: "inner".into(),
            kind: SymbolKind::Function,
            start_line: 10,
            end_line: 20,
            parent: Some("outer".into()),
        };

        assert!(outer.contains_line(5));
        assert!(outer.contains_line(30));
        assert!(inner.contains_line(15));
        assert!(!inner.contains_line(5));
        assert!(inner.is_inner_of(&outer));
        assert!(!outer.is_inner_of(&inner));
    }

    #[test]
    fn list_symbols_filters_by_file_and_range() {
        let file = PathBuf::from("src/example.rs");
        let other = PathBuf::from("src/other.rs");

        let mut repo = RepoMap::default();
        repo.symbols.push(make_symbol(
            "ok_fn",
            &file,
            10,
            20,
            None,
            SymbolKind::Function,
        ));
        // invalid range
        repo.symbols.push(make_symbol(
            "bad_fn",
            &file,
            30,
            29,
            None,
            SymbolKind::Function,
        ));
        // different file
        repo.symbols.push(make_symbol(
            "other_fn",
            &other,
            5,
            10,
            None,
            SymbolKind::Function,
        ));

        let symbols = list_symbols(&repo, &file).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "ok_fn");
    }

    #[test]
    fn find_enclosing_symbol_picks_innermost() {
        let file = PathBuf::from("src/example.rs");
        let mut repo = RepoMap::default();

        repo.symbols.push(make_symbol(
            "outer",
            &file,
            5,
            50,
            None,
            SymbolKind::Function,
        ));
        repo.symbols.push(make_symbol(
            "inner",
            &file,
            10,
            20,
            Some("outer"),
            SymbolKind::Function,
        ));

        let sym = find_enclosing_symbol(&repo, &file, 15)
            .unwrap()
            .expect("symbol expected");

        assert_eq!(sym.name, "inner");
    }
}
