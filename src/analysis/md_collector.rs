use crate::analysis::{LanguageSpecificExtractor, RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::{Node, Tree};

pub struct MarkdownExtractor;

impl LanguageSpecificExtractor for MarkdownExtractor {
    fn extract_symbols(
        &self,
        map: &mut RepoMap,
        tree: &Tree,
        src: &str,
        file: &Path,
    ) -> Result<()> {
        let root = tree.root_node();
        let file_total_lines = src.lines().count();

        // Traverse the tree to extract headings as symbols
        let mut cursor = root.walk();
        visit_md_node(map, &mut cursor, src, file, file_total_lines);
        Ok(())
    }
}

fn visit_md_node(
    map: &mut RepoMap,
    cursor: &mut tree_sitter::TreeCursor,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    let node = cursor.node();
    if node.kind() == "heading" {
        // Get the heading text
        let name = node_text(node, src).trim().to_string();
        if name.is_empty() {
            return;
        }

        // Determine kind based on heading level
        let level = node.child_count(); // Approximate level by child count of hash
        let kind = match level {
            1 => SymbolKind::Mod,          // # Top-level sections
            2 => SymbolKind::Struct,       // ## Subsections
            3..=6 => SymbolKind::Variable, // ### Smaller sections
            _ => SymbolKind::Variable,
        };

        let symbol_info = SymbolInfo {
            name,
            kind,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: None,
            keywords: vec![], // Could extract from inline comments if needed
        };
        map.symbols.push(symbol_info);
    }

    // Recurse on named children
    if cursor.goto_first_child() {
        loop {
            visit_md_node(map, cursor, src, file, file_total_lines);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn node_text(node: Node, src: &str) -> String {
    node.utf8_text(src.as_bytes())
        .map(|s| s.to_string())
        .unwrap_or_default()
}
