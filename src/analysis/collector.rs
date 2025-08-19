use crate::analysis::RepoMap;
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

// Helper functions (kept generic)
pub(super) fn node_text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

pub(super) fn name_from(node: Node, field: &str, src: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(n, src).to_string())
}

// Trait for language-specific symbol extraction
pub trait LanguageSpecificExtractor: Send + Sync {
    fn extract_symbols(
        &self,
        map: &mut RepoMap,
        tree: &tree_sitter::Tree,
        src: &str,
        file: &Path,
    ) -> Result<()>;
}
