use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Method,
    AssocFn,
    Mod,
    Variable,
    Comment,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Method => "method",
            SymbolKind::AssocFn => "assoc_fn",
            SymbolKind::Mod => "mod",
            SymbolKind::Variable => "var",
            SymbolKind::Comment => "comment",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub parent: Option<String>,
    /// Total number of lines in the file
    pub file_total_lines: usize,
    /// Number of lines in the function (only for functions)
    pub function_lines: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoMap {
    pub symbols: Vec<SymbolInfo>,
}

impl RepoMap {
    // Merge multiple RepoMaps into a single one
    pub fn merge(mut self, other: RepoMap) -> Self {
        self.symbols.extend(other.symbols);
        self
    }

    // Function to combine Vec<RepoMap>
    pub fn merge_many(maps: Vec<RepoMap>) -> Self {
        maps.into_iter()
            .reduce(|acc, map| acc.merge(map))
            .unwrap_or_default()
    }
}
