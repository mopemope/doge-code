use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{LanguageSpecificExtractor, name_from, node_text, push_symbol};

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
        visit_py_node(map, root, src, file, None);
        Ok(())
    }
}

fn visit_py_node(map: &mut RepoMap, node: Node, src: &str, file: &Path, class_ctx: Option<String>) {
    let file_total_lines = src.lines().count();

    match node.kind() {
        "function_definition" => {
            handle_function_definition(map, node, src, file, &class_ctx, file_total_lines)
        }
        "class_definition" => {
            if let Some(name) = name_from(node, "name", src) {
                handle_class_definition(map, node, src, file, file_total_lines, &name);
                let mut c = node.walk();
                if c.goto_first_child() {
                    loop {
                        visit_py_node(map, c.node(), src, file, Some(name.clone()));
                        if !c.goto_next_sibling() {
                            break;
                        }
                    }
                    c.goto_parent();
                }
                return;
            }
        }
        "assignment" => handle_assignment(map, node, src, file, file_total_lines),
        _ => {}
    }

    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            visit_py_node(map, c.node(), src, file, class_ctx.clone());
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
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_class_definition(
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

fn handle_assignment(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    if let Some(lhs) = node.child_by_field_name("left") {
        if lhs.kind() == "identifier" {
            let name = node_text(lhs, src).to_string();
            let symbol_info = SymbolInfo {
                name,
                kind: SymbolKind::Variable,
                file: file.to_path_buf(),
                start_line: lhs.start_position().row + 1,
                start_col: lhs.start_position().column + 1,
                end_line: lhs.end_position().row + 1,
                end_col: lhs.end_position().column + 1,
                parent: None,
                file_total_lines,
                function_lines: None,
            };
            push_symbol(map, symbol_info);
        } else if lhs.kind() == "pattern_list" || lhs.kind() == "tuple_pattern" {
            extract_identifiers_from_py_lhs(map, lhs, src, file, file_total_lines);
        }
    }
}

fn extract_identifiers_from_py_lhs(
    map: &mut RepoMap,
    lhs_node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    if lhs_node.kind() == "identifier" {
        let name = node_text(lhs_node, src).to_string();
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Variable,
            file: file.to_path_buf(),
            start_line: lhs_node.start_position().row + 1,
            start_col: lhs_node.start_position().column + 1,
            end_line: lhs_node.end_position().row + 1,
            end_col: lhs_node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: None,
        };
        push_symbol(map, symbol_info);
    } else {
        let mut c = lhs_node.walk();
        if c.goto_first_child() {
            loop {
                extract_identifiers_from_py_lhs(map, c.node(), src, file, file_total_lines);
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }
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
