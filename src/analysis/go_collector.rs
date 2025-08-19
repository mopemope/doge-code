use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{LanguageSpecificExtractor, name_from, node_text, push_symbol};

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
        visit_go_node(map, root, src, file, None);
        Ok(())
    }
}

fn visit_go_node(map: &mut RepoMap, node: Node, src: &str, file: &Path, recv_ctx: Option<String>) {
    let file_total_lines = src.lines().count();

    match node.kind() {
        "function_declaration" => {
            handle_function_declaration(map, node, src, file, file_total_lines)
        }
        "method_declaration" => handle_method_declaration(map, node, src, file, file_total_lines),
        "type_declaration" => handle_type_declaration(map, node, src, file, file_total_lines),
        "const_declaration" | "var_declaration" => {
            handle_const_or_var_declaration(map, node, src, file, file_total_lines)
        }
        _ => {}
    }

    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            visit_go_node(map, c.node(), src, file, recv_ctx.clone());
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
) {
    if let Some(name) = name_from(node, "name", src) {
        let function_lines = node.end_position().row - node.start_position().row + 1;
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
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_method_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
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
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_type_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
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
                };
                push_symbol(map, symbol_info);
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
}

fn handle_const_or_var_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            let child = c.node();
            if (child.kind() == "const_spec" || child.kind() == "var_spec")
                && let Some(name_node) = child.child_by_field_name("name")
            {
                let name = node_text(name_node, src).to_string();
                let symbol_info = SymbolInfo {
                    name,
                    kind: SymbolKind::Variable,
                    file: file.to_path_buf(),
                    start_line: name_node.start_position().row + 1,
                    start_col: name_node.start_position().column + 1,
                    end_line: name_node.end_position().row + 1,
                    end_col: name_node.end_position().column + 1,
                    parent: None,
                    file_total_lines,
                    function_lines: None,
                };
                push_symbol(map, symbol_info);
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
        c.goto_parent();
    }
}
