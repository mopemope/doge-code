use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;

use crate::analysis::{Analyzer, RepoMap, SymbolInfo};

#[derive(Debug, Clone, Serialize)]
pub struct SymbolQueryResult {
    pub name: String,
    pub kind: String,
    pub file: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub parent: Option<String>,
}

impl From<SymbolInfo> for SymbolQueryResult {
    fn from(s: SymbolInfo) -> Self {
        Self {
            name: s.name,
            kind: s.kind.as_str().to_string(),
            file: s.file,
            start_line: s.start_line,
            end_line: s.end_line,
            parent: s.parent,
        }
    }
}

pub struct SymbolTools;

impl Default for SymbolTools {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTools {
    pub fn new() -> Self {
        Self
    }

    // This function is hard to test directly without a complex setup.
    // We will test the filtering logic in a separate, testable function.
    pub fn get_symbol_info(
        &self,
        query: &str,
        include: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<SymbolQueryResult>> {
        // TODO: Analyzer の初期化方法を変更する必要があります。
        // 現在の実装では、Analyzer がプロジェクトルートパスを必要としています。
        // しかし、Analyzer の実装が不明なため、この修正を保留します。
        let mut analyzer = Analyzer::new(".")?;
        let map: RepoMap = analyzer.build()?;
        let results = Self::filter_symbols(map.symbols, query, include, kind);
        Ok(results)
    }

    fn filter_symbols(
        symbols: Vec<SymbolInfo>,
        query: &str,
        include: Option<&str>,
        kind: Option<&str>,
    ) -> Vec<SymbolQueryResult> {
        let mut out = Vec::new();
        for s in symbols {
            if let Some(glob_like) = include {
                if !s.file.to_string_lossy().contains(glob_like) {
                    continue;
                }
            }
            if !s.name.contains(query) {
                continue;
            }
            if let Some(k) = kind {
                if s.kind.as_str() != k {
                    continue;
                }
            }
            out.push(SymbolQueryResult::from(s));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SymbolKind;
    use std::path::PathBuf;

    fn create_dummy_symbol(name: &str, kind: SymbolKind, file: &str) -> SymbolInfo {
        SymbolInfo {
            name: name.to_string(),
            kind,
            file: PathBuf::from(file),
            start_line: 1,
            end_line: 10,
            parent: None,
        }
    }

    #[test]
    fn test_filter_symbols_by_name() {
        let symbols = vec![
            create_dummy_symbol("my_function", SymbolKind::Function, "src/main.rs"),
            create_dummy_symbol("another_function", SymbolKind::Function, "src/lib.rs"),
        ];
        let results = SymbolTools::filter_symbols(symbols, "my_function", None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "my_function");
    }

    #[test]
    fn test_filter_symbols_by_kind() {
        let symbols = vec![
            create_dummy_symbol("my_function", SymbolKind::Function, "src/main.rs"),
            create_dummy_symbol("my_struct", SymbolKind::Struct, "src/main.rs"),
        ];
        let results = SymbolTools::filter_symbols(symbols, "my", None, Some("fn"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "fn");
    }

    #[test]
    fn test_filter_symbols_by_include() {
        let symbols = vec![
            create_dummy_symbol("main_func", SymbolKind::Function, "src/main.rs"),
            create_dummy_symbol("lib_func", SymbolKind::Function, "src/lib.rs"),
        ];
        let results = SymbolTools::filter_symbols(symbols, "func", Some("lib.rs"), None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "lib_func");
    }
}
