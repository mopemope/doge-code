use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{LanguageSpecificExtractor, name_from, node_text, push_symbol};

// ---------------- TypeScript/JavaScript Extractor -----------------
pub struct TypeScriptExtractor;
pub struct JavaScriptExtractor;

impl LanguageSpecificExtractor for TypeScriptExtractor {
    fn extract_symbols(
        &self,
        map: &mut RepoMap,
        tree: &tree_sitter::Tree,
        src: &str,
        file: &Path,
    ) -> Result<()> {
        collect_ts_js(map, tree, src, file, true);
        Ok(())
    }
}

impl LanguageSpecificExtractor for JavaScriptExtractor {
    fn extract_symbols(
        &self,
        map: &mut RepoMap,
        tree: &tree_sitter::Tree,
        src: &str,
        file: &Path,
    ) -> Result<()> {
        collect_ts_js(map, tree, src, file, false);
        Ok(())
    }
}

fn collect_ts_js(
    map: &mut RepoMap,
    tree: &tree_sitter::Tree,
    src: &str,
    file: &Path,
    _is_ts: bool,
) {
    let root = tree.root_node();
    let mut cursor = root.walk();
    if cursor.goto_first_child() {
        loop {
            visit_ts_js(map, cursor.node(), src, file, None);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn visit_ts_js(map: &mut RepoMap, node: Node, src: &str, file: &Path, class_ctx: Option<String>) {
    let file_total_lines = src.lines().count();

    match node.kind() {
        "function_declaration" => {
            handle_function_declaration(map, node, src, file, file_total_lines)
        }
        "class_declaration" => {
            if let Some(name) = name_from(node, "name", src) {
                handle_class_declaration(map, node, src, file, file_total_lines, &name);
                let mut c = node.walk();
                if c.goto_first_child() {
                    loop {
                        visit_ts_js(map, c.node(), src, file, Some(name.clone()));
                        if !c.goto_next_sibling() {
                            break;
                        }
                    }
                    c.goto_parent();
                }
                return;
            }
        }
        "method_definition" => {
            handle_method_definition(map, node, src, file, &class_ctx, file_total_lines)
        }
        "enum_declaration" => handle_enum_declaration(map, node, src, file, file_total_lines),
        "interface_declaration" => {
            handle_interface_declaration(map, node, src, file, file_total_lines)
        }
        "lexical_declaration" | "variable_declaration" => {
            handle_lexical_or_variable_declaration(map, node, src, file, file_total_lines)
        }
        _ => {}
    }

    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            visit_ts_js(map, c.node(), src, file, class_ctx.clone());
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

fn handle_class_declaration(
    map: &mut RepoMap,
    node: Node,
    _src: &str,
    file: &Path,
    file_total_lines: usize,
    name: &str,
) {
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
    };
    push_symbol(map, symbol_info);
}

fn handle_method_definition(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    class_ctx: &Option<String>,
    file_total_lines: usize,
) {
    if let Some(name_node) = node.child_by_field_name("name") {
        let name = node_text(name_node, src).to_string();
        let function_lines = node.end_position().row - node.start_position().row + 1;
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Method,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: class_ctx.clone(),
            file_total_lines,
            function_lines: Some(function_lines),
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_enum_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    if let Some(name) = name_from(node, "name", src) {
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
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_interface_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    if let Some(name) = name_from(node, "name", src) {
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Trait,
            file: file.to_path_buf(),
            start_line: node.start_position().row + 1,
            start_col: node.start_position().column + 1,
            end_line: node.end_position().row + 1,
            end_col: node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: None,
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_lexical_or_variable_declaration(
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
            if child.kind() == "variable_declarator"
                && let Some(id_node) = child.child_by_field_name("name") {
                    let name = node_text(id_node, src).to_string();
                    let symbol_info = SymbolInfo {
                        name,
                        kind: SymbolKind::Variable,
                        file: file.to_path_buf(),
                        start_line: id_node.start_position().row + 1,
                        start_col: id_node.start_position().column + 1,
                        end_line: id_node.end_position().row + 1,
                        end_col: id_node.end_position().column + 1,
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
