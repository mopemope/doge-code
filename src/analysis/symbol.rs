use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// Keywords extracted from comments associated with this symbol
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RelationType {
    Call,
    Use,
    Implements,
    Inherits,
    Import,
}

impl RelationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            RelationType::Call => "call",
            RelationType::Use => "use",
            RelationType::Implements => "implements",
            RelationType::Inherits => "inherits",
            RelationType::Import => "import",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRelation {
    pub source_symbol_name: String, // 名前で一時的にリンク (ID解決は保存時または事後)
    pub source_symbol_parent: Option<String>, // 親スコープ (impl名など)
    pub source_file_path: PathBuf,  // ID解決のためにファイルのパスが必要
    pub target_symbol_name: String,
    pub relation_type: RelationType,
    pub line: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoMap {
    pub symbols: Vec<SymbolInfo>,
    #[serde(default)]
    pub relations: Vec<SymbolRelation>,
}

impl RepoMap {
    // Merge multiple RepoMaps into a single one
    pub fn merge(mut self, other: RepoMap) -> Self {
        self.symbols.extend(other.symbols);
        self.relations.extend(other.relations);
        self
    }

    // Function to combine Vec<RepoMap>
    pub fn merge_many(maps: Vec<RepoMap>) -> Self {
        maps.into_iter()
            .reduce(|acc, map| acc.merge(map))
            .unwrap_or_default()
    }

    /// Replace the analysis for a single file.
    ///
    /// Removes all symbols with `symbol.file == file` and all relations with
    /// `source_file_path == file`, then appends the replacement. Used after a
    /// successful semantic edit so the shared map stops serving stale ranges.
    pub fn replace_file(&mut self, file: &Path, replacement: RepoMap) {
        self.symbols
            .retain(|symbol| !paths_equal(&symbol.file, file));
        self.relations
            .retain(|relation| !paths_equal(&relation.source_file_path, file));
        self.symbols.extend(replacement.symbols);
        self.relations.extend(replacement.relations);
    }

    /// Remove all symbols/relations sourced from `file`.
    pub fn remove_file(&mut self, file: &Path) {
        self.symbols
            .retain(|symbol| !paths_equal(&symbol.file, file));
        self.relations
            .retain(|relation| !paths_equal(&relation.source_file_path, file));
    }
}

/// Compare paths with a canonicalized fallback so symlinked roots
/// (e.g. `/tmp` vs `/private/tmp` on macOS) still match.
fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (a.canonicalize(), b.canonicalize()) {
        return ca == cb;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_replace_file_swaps_only_target() {
        let mut map = RepoMap {
            symbols: vec![sym("a", "a.rs"), sym("b", "b.rs")],
            relations: vec![],
        };
        let replacement = RepoMap {
            symbols: vec![sym("a2", "a.rs")],
            relations: vec![],
        };
        map.replace_file(Path::new("a.rs"), replacement);
        assert_eq!(map.symbols.len(), 2);
        assert!(map.symbols.iter().any(|s| s.name == "a2"));
        assert!(map.symbols.iter().any(|s| s.name == "b"));
        assert!(!map.symbols.iter().any(|s| s.name == "a"));
    }
}
