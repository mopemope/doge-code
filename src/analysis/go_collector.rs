use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{
    LanguageSpecificExtractor, extract_keywords_from_comment, name_from, node_text,
};

// ---------------- Go Extractor -----------------
pub struct GoExtractor;

impl LanguageSpecificExtractor for GoExtractor {
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
        visit_go_node(map, root, src, file, None, &comments);
        Ok(())
    }
}

/// Collect all comments in the file with their positions
fn collect_comments(node: Node, src: &str, comments: &mut Vec<(usize, String)>) {
    if node.kind() == "comment" {
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

/// Find comments that are associated with a node (before or near it)
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

fn visit_go_node(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    recv_ctx: Option<String>,
    comments: &[(usize, String)],
) {
    let file_total_lines = src.lines().count();

    match node.kind() {
        "function_declaration" => {
            handle_function_declaration(map, node, src, file, file_total_lines, comments)
        }
        "method_declaration" => {
            handle_method_declaration(map, node, src, file, file_total_lines, comments)
        }
        "type_declaration" => {
            handle_type_declaration(map, node, src, file, file_total_lines, comments)
        }
        "comment" => handle_comment(map, node, src, file, file_total_lines),
        _ => {}
    }

    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            visit_go_node(map, c.node(), src, file, recv_ctx.clone(), comments);
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
}

fn handle_function_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    if let Some(name) = name_from(node, "name", src) {
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

fn handle_method_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    let mut receiver_type = None;
    if let Some(receiver_node) = node.child_by_field_name("receiver") {
        let mut cursor = receiver_node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "parameter_declaration"
                    && let Some(type_node) = child.child_by_field_name("type")
                {
                    receiver_type = Some(node_text(type_node, src).to_string());
                    break;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }

    if let Some(name) = name_from(node, "name", src) {
        let function_lines = node.end_position().row - node.start_position().row + 1;
        let keywords = find_associated_comments(node, comments);
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Method,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: receiver_type,
            file_total_lines,
            function_lines: Some(function_lines),
            keywords,
        };
        map.symbols.push(symbol_info);
    }
}

fn handle_type_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            if child.kind() == "type_spec"
                && let Some(name) = name_from(child, "name", src)
            {
                let type_node = child.child_by_field_name("type");
                let kind = if let Some(tn) = type_node {
                    match tn.kind() {
                        "struct_type" => SymbolKind::Struct,
                        "interface_type" => SymbolKind::Trait,
                        _ => SymbolKind::Struct,
                    }
                } else {
                    SymbolKind::Struct
                };
                let keywords = find_associated_comments(child, comments);
                let symbol_info = SymbolInfo {
                    name: name.clone(),
                    kind,
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
