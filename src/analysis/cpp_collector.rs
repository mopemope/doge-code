use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{LanguageSpecificExtractor, extract_keywords_from_comment, node_text};

// ---------------- C++ Extractor -----------------
pub struct CppExtractor;

impl LanguageSpecificExtractor for CppExtractor {
    fn extract_symbols(
        &self,
        map: &mut RepoMap,
        tree: &tree_sitter::Tree,
        src: &str,
        file: &Path,
    ) -> Result<()> {
        let root = tree.root_node();

        // First pass: collect all comments and their positions
        let mut comments = Vec::new();
        collect_comments(root, src, &mut comments);

        // Second pass: extract symbols and associate keywords
        visit_cpp_node(map, root, src, file, &comments);
        Ok(())
    }
}

/// Collect all comments in the file with their positions.
fn collect_comments(node: Node, src: &str, comments: &mut Vec<(usize, String)>) {
    if node.kind() == "comment" || node.kind() == "line_comment" || node.kind() == "block_comment" {
        let comment_text = node_text(node, src).to_string();
        comments.push((node.start_position().row, comment_text));
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_comments(cursor.node(), src, comments);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Find comments that are associated with a node (before or near it).
fn find_associated_comments(node: Node, comments: &[(usize, String)]) -> Vec<String> {
    let node_start_line = node.start_position().row;
    let mut keywords = Vec::new();

    // Look for comments within 3 lines before the node
    for (line, comment) in comments {
        if *line < node_start_line && node_start_line - line <= 3 {
            keywords.extend(extract_keywords_from_comment(comment));
        }
    }

    keywords
}

fn visit_cpp_node(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    comments: &[(usize, String)],
) {
    let file_total_lines = src.lines().count();

    match node.kind() {
        "function_definition" => {
            handle_function_definition(map, node, src, file, file_total_lines, comments)
        }
        "struct_specifier" => {
            handle_struct_specifier(map, node, src, file, file_total_lines, comments)
        }
        "class_specifier" => {
            handle_class_specifier(map, node, src, file, file_total_lines, comments)
        }
        "enum_specifier" => handle_enum_specifier(map, node, src, file, file_total_lines, comments),
        "declaration" => handle_declaration(map, node, src, file, file_total_lines, comments),
        "comment" | "line_comment" | "block_comment" => {
            handle_comment(map, node, src, file, file_total_lines)
        }
        _ => {}
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            visit_cpp_node(map, cursor.node(), src, file, comments);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn handle_function_definition(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    if let Some(name) = {
        node.child_by_field_name("declarator")
            .and_then(|d| first_identifier(d, src))
            .or_else(|| first_identifier(node, src))
    } {
        let function_lines = node.end_position().row - node.start_position().row + 1;
        let keywords = find_associated_comments(node, comments);
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Function,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: Some(function_lines),
            keywords,
        };
        map.symbols.push(symbol_info);
    }
}

fn handle_struct_specifier(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    let mut name = None;
    if let Some(n) = node.child_by_field_name("name") {
        name = Some(node_text(n, src).to_string());
    }
    if name.is_none() {
        name = first_identifier(node, src);
    }

    if let Some(name) = name {
        let keywords = find_associated_comments(node, comments);
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Struct,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: None,
            keywords,
        };
        map.symbols.push(symbol_info);
    }
}

fn handle_class_specifier(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    let mut name = None;
    if let Some(n) = node.child_by_field_name("name") {
        name = Some(node_text(n, src).to_string());
    }
    if name.is_none() {
        name = first_identifier(node, src);
    }

    if let Some(name) = name {
        let keywords = find_associated_comments(node, comments);
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Struct,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: None,
            keywords,
        };
        map.symbols.push(symbol_info);
    }
}

fn handle_enum_specifier(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    let mut name = None;
    if let Some(n) = node.child_by_field_name("name") {
        name = Some(node_text(n, src).to_string());
    }
    if name.is_none() {
        name = first_identifier(node, src);
    }

    if let Some(name) = name {
        let keywords = find_associated_comments(node, comments);
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Enum,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: None,
            keywords,
        };
        map.symbols.push(symbol_info);
    }
}

fn handle_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    // Look for declarators inside the declaration
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            if (child.kind() == "init_declarator" || child.kind() == "declarator")
                && let Some(id) = first_identifier(child, src)
            {
                let keywords = find_associated_comments(child, comments);
                let symbol_info = SymbolInfo {
                    name: id,
                    kind: SymbolKind::Variable,
                    file: file.to_path_buf(),
                    start_line: child.start_position().row + 1,
                    start_col: child.start_position().column + 1,
                    end_line: child.end_position().row + 1,
                    end_col: child.end_position().column + 1,
                    parent: None,
                    file_total_lines,
                    function_lines: None,
                    keywords,
                };
                map.symbols.push(symbol_info);
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
}

fn handle_comment(map: &mut RepoMap, node: Node, src: &str, file: &Path, file_total_lines: usize) {
    let name = node_text(node, src).to_string();
    // For comments, we extract keywords directly from the comment text
    let keywords = extract_keywords_from_comment(&name);
    let symbol_info = SymbolInfo {
        name,
        kind: SymbolKind::Comment,
        file: file.to_path_buf(),
        start_line: node.start_position().row + 1,
        start_col: node.start_position().column + 1,
        end_line: node.end_position().row + 1,
        end_col: node.end_position().column + 1,
        parent: None,
        file_total_lines,
        function_lines: None,
        keywords,
    };
    map.symbols.push(symbol_info);
}

fn first_identifier(node: Node, src: &str) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(node_text(node, src).to_string());
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            if child.kind() == "identifier" {
                return Some(node_text(child, src).to_string());
            }
            if let Some(found) = first_identifier(child, src) {
                return Some(found);
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
    None
}
