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

pub struct SymbolTools {
    root: PathBuf,
}

impl SymbolTools {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn get_symbol_info(
        &self,
        query: &str,
        include: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Vec<SymbolQueryResult>> {
        // Build a fresh map for now (later: cache)
        let mut analyzer = Analyzer::new(&self.root)?;
        let map: RepoMap = analyzer.build()?;
        let mut out = Vec::new();
        for s in map.symbols.into_iter() {
            // include glob is applied at analysis time today; to keep simple, filter by file path contains
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
        Ok(out)
    }
}
