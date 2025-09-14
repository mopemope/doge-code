use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{
    LanguageSpecificExtractor, extract_keywords_from_comment, name_from, node_text,
};

// ---------------- C# Extractor -----------------
pub struct CSharpExtractor;

impl LanguageSpecificExtractor for CSharpExtractor {
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
        visit_csharp_node(map, root, src, file, None, &comments);
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

    for (line, comment) in comments {
        if *line < node_start_line && node_start_line - line <= 3 {
            keywords.extend(extract_keywords_from_comment(comment));
        }
    }

    keywords
}

fn visit_csharp_node(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    type_ctx: Option<String>,
    comments: &[(usize, String)],
) {
    let file_total_lines = src.lines().count();

    match node.kind() {
        "namespace_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
                let keywords = find_associated_comments(node, comments);
                let symbol_info = SymbolInfo {
                    name,
                    kind: SymbolKind::Mod,
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
        // Treat records as structs and recurse into their bodies like classes/structs
        "class_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "record_struct_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
                let keywords = find_associated_comments(node, comments);
                let symbol_info = SymbolInfo {
                    name: name.clone(),
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

                // Walk class/struct/record body with type context
                let mut c = node.walk();
                if c.goto_first_child() {
                    loop {
                        visit_csharp_node(map, c.node(), src, file, Some(name.clone()), comments);
                        if !c.goto_next_sibling() {
                            break;
                        }
                    }
                    c.goto_parent();
                }
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
                let keywords = find_associated_comments(node, comments);
                let symbol_info = SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::Trait,
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

                let mut c = node.walk();
                if c.goto_first_child() {
                    loop {
                        visit_csharp_node(map, c.node(), src, file, Some(name.clone()), comments);
                        if !c.goto_next_sibling() {
                            break;
                        }
                    }
                    c.goto_parent();
                }
                return;
            }
        }
        "enum_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
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
        // Delegates behave like function signatures; treat as functions
        "delegate_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
                let keywords = find_associated_comments(node, comments);
                let symbol_info = SymbolInfo {
                    name,
                    kind: SymbolKind::Function,
                    file: file.to_path_buf(),
                    start_line: node.start_position().row + 1,
                    start_col: node.start_position().column + 1,
                    end_line: node.end_position().row + 1,
                    end_col: node.end_position().column + 1,
                    parent: type_ctx.clone(),
                    file_total_lines,
                    function_lines: None,
                    keywords,
                };
                map.symbols.push(symbol_info);
            }
        }
        // Events
        "event_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
                let keywords = find_associated_comments(node, comments);
                let symbol_info = SymbolInfo {
                    name,
                    kind: SymbolKind::Variable,
                    file: file.to_path_buf(),
                    start_line: node.start_position().row + 1,
                    start_col: node.start_position().column + 1,
                    end_line: node.end_position().row + 1,
                    end_col: node.end_position().column + 1,
                    parent: type_ctx.clone(),
                    file_total_lines,
                    function_lines: None,
                    keywords,
                };
                map.symbols.push(symbol_info);
            }
        }
        "event_field_declaration" => {
            // Similar to field_declaration but for event fields which can include multiple declarators
            let mut c = node.walk();
            if c.goto_first_child() {
                loop {
                    let child = c.node();
                    if child.kind() == "variable_declaration" {
                        let mut dc = child.walk();
                        if dc.goto_first_child() {
                            loop {
                                let d = dc.node();
                                if d.kind() == "variable_declarator"
                                    && let Some(id) = d
                                        .child_by_field_name("name")
                                        .map(|n| node_text(n, src).to_string())
                                        .or_else(|| first_identifier(d, src))
                                {
                                    let keywords = find_associated_comments(node, comments);
                                    let symbol_info = SymbolInfo {
                                        name: id,
                                        kind: SymbolKind::Variable,
                                        file: file.to_path_buf(),
                                        start_line: d.start_position().row + 1,
                                        start_col: d.start_position().column + 1,
                                        end_line: d.end_position().row + 1,
                                        end_col: d.end_position().column + 1,
                                        parent: type_ctx.clone(),
                                        file_total_lines,
                                        function_lines: None,
                                        keywords,
                                    };
                                    map.symbols.push(symbol_info);
                                }
                                if !dc.goto_next_sibling() {
                                    break;
                                }
                            }
                            dc.goto_parent();
                        }
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
                c.goto_parent();
            }
        }
        "method_declaration" | "constructor_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
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
                    parent: type_ctx.clone(),
                    file_total_lines,
                    function_lines: Some(function_lines),
                    keywords,
                };
                map.symbols.push(symbol_info);
            }
        }
        "property_declaration" => {
            if let Some(name) = name_from(node, "name", src).or_else(|| first_identifier(node, src))
            {
                let keywords = find_associated_comments(node, comments);
                let symbol_info = SymbolInfo {
                    name,
                    kind: SymbolKind::Variable,
                    file: file.to_path_buf(),
                    start_line: node.start_position().row + 1,
                    start_col: node.start_position().column + 1,
                    end_line: node.end_position().row + 1,
                    end_col: node.end_position().column + 1,
                    parent: type_ctx.clone(),
                    file_total_lines,
                    function_lines: None,
                    keywords,
                };
                map.symbols.push(symbol_info);
            }
        }
        "field_declaration" => {
            // Capture each variable declarator under this field declaration
            let mut c = node.walk();
            if c.goto_first_child() {
                loop {
                    let child = c.node();
                    if child.kind() == "variable_declaration" {
                        let mut dc = child.walk();
                        if dc.goto_first_child() {
                            loop {
                                let d = dc.node();
                                if d.kind() == "variable_declarator"
                                    && let Some(id) = d
                                        .child_by_field_name("name")
                                        .map(|n| node_text(n, src).to_string())
                                        .or_else(|| first_identifier(d, src))
                                {
                                    let keywords = find_associated_comments(node, comments);
                                    let symbol_info = SymbolInfo {
                                        name: id,
                                        kind: SymbolKind::Variable,
                                        file: file.to_path_buf(),
                                        start_line: d.start_position().row + 1,
                                        start_col: d.start_position().column + 1,
                                        end_line: d.end_position().row + 1,
                                        end_col: d.end_position().column + 1,
                                        parent: type_ctx.clone(),
                                        file_total_lines,
                                        function_lines: None,
                                        keywords,
                                    };
                                    map.symbols.push(symbol_info);
                                }
                                if !dc.goto_next_sibling() {
                                    break;
                                }
                            }
                            dc.goto_parent();
                        }
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
                c.goto_parent();
            }
        }
        "comment" | "line_comment" | "block_comment" => {
            handle_comment(map, node, src, file, file_total_lines)
        }
        _ => {}
    }

    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            visit_csharp_node(map, c.node(), src, file, type_ctx.clone(), comments);
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
}

fn first_identifier(node: Node, src: &str) -> Option<String> {
    // Fallback: find the first identifier under this node
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            if child.kind() == "identifier" {
                return Some(node_text(child, src).to_string());
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
    None
}

fn handle_comment(map: &mut RepoMap, node: Node, src: &str, file: &Path, file_total_lines: usize) {
    let name = node_text(node, src).to_string();
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
