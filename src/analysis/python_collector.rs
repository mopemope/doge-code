use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{
    LanguageSpecificExtractor, extract_keywords_from_comment, name_from, node_text,
};

// ---------------- Python Extractor -----------------
pub struct PythonExtractor;

impl LanguageSpecificExtractor for PythonExtractor {
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
        visit_py_node(map, root, src, file, None, &comments);
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

fn visit_py_node(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    class_ctx: Option<String>,
    comments: &[(usize, String)],
) {
    let file_total_lines = src.lines().count();

    match node.kind() {
        "function_definition" => {
            handle_function_definition(map, node, src, file, &class_ctx, file_total_lines, comments)
        }
        "class_definition" => {
            if let Some(name) = name_from(node, "name", src) {
                handle_class_definition(map, node, src, file, file_total_lines, &name, comments);
                let mut c = node.walk();
                if c.goto_first_child() {
                    loop {
                        visit_py_node(map, c.node(), src, file, Some(name.clone()), comments);
                        if !c.goto_next_sibling() {
                            break;
                        }
                    }
                    c.goto_parent();
                }
                return;
            }
        }
        "comment" => handle_comment(map, node, src, file, file_total_lines),
        _ => {}
    }

    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            visit_py_node(map, c.node(), src, file, class_ctx.clone(), comments);
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
}

fn handle_function_definition(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    class_ctx: &Option<String>,
    file_total_lines: usize,
    comments: &[(usize, String)],
) {
    if let Some(name) = name_from(node, "name", src) {
        let is_method = class_ctx.is_some() && first_param_is_self_or_cls(node, src);
        let kind = if is_method {
            SymbolKind::Method
        } else if class_ctx.is_some() {
            SymbolKind::AssocFn
        } else {
            SymbolKind::Function
        };
        let function_lines = node.end_position().row - node.start_position().row + 1;
        let keywords = find_associated_comments(node, comments);
        let symbol_info = SymbolInfo {
            name,
            kind,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: class_ctx.clone(),
            file_total_lines,
            function_lines: Some(function_lines),
            keywords,
        };
        map.symbols.push(symbol_info);
    }
}

fn handle_class_definition(
    map: &mut RepoMap,
    node: Node,
    _src: &str,
    file: &Path,
    file_total_lines: usize,
    name: &str,
    comments: &[(usize, String)],
) {
    let keywords = find_associated_comments(node, comments);
    let symbol_info = SymbolInfo {
        name: name.to_string(),
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

fn first_param_is_self_or_cls(fn_node: Node, src: &str) -> bool {
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut c = params.walk();
        if c.goto_first_child() {
            loop {
                let child = c.node();
                if child.kind() == "identifier" {
                    let name = node_text(child, src);
                    return name == "self" || name == "cls";
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }
    false
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
