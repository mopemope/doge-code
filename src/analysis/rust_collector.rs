use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

use super::collector::{LanguageSpecificExtractor, name_from, node_text, push_symbol};

// ---------------- Rust Extractor -----------------
pub struct RustExtractor;

impl LanguageSpecificExtractor for RustExtractor {
    fn extract_symbols(
        &self,
        map: &mut RepoMap,
        tree: &tree_sitter::Tree,
        src: &str,
        file: &Path,
    ) -> Result<()> {
        let root = tree.root_node();
        let file_total_lines = src.lines().count();
        visit_rust_node(map, root, src, file, None, file_total_lines);
        Ok(())
    }
}

fn visit_rust_node(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    ctx_impl: Option<String>,
    file_total_lines: usize,
) {
    match node.kind() {
        "function_item" => handle_function_item(map, node, src, file, file_total_lines),
        "struct_item" => handle_struct_item(map, node, src, file, file_total_lines),
        "enum_item" => handle_enum_item(map, node, src, file, file_total_lines),
        "trait_item" => handle_trait_item(map, node, src, file, file_total_lines),
        "mod_item" => handle_mod_item(map, node, src, file, file_total_lines),
        "let_declaration" => handle_let_declaration(map, node, src, file, file_total_lines),
        "impl_item" => handle_impl_item(map, node, src, file, file_total_lines),
        _ => {}
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            visit_rust_node(
                map,
                cursor.node(),
                src,
                file,
                ctx_impl.clone(),
                file_total_lines,
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn handle_function_item(
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

fn handle_struct_item(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    if let Some(name) = name_from(node, "name", src) {
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
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_enum_item(
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

fn handle_trait_item(
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

fn handle_mod_item(map: &mut RepoMap, node: Node, src: &str, file: &Path, file_total_lines: usize) {
    if let Some(name) = name_from(node, "name", src) {
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
        };
        push_symbol(map, symbol_info);
    }
}

fn handle_let_declaration(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    if let Some(pattern) = node.child_by_field_name("pattern") {
        if pattern.kind() == "identifier" {
            let name = node_text(pattern, src).to_string();
            let symbol_info = SymbolInfo {
                name,
                kind: SymbolKind::Variable,
                file: file.to_path_buf(),
                start_line: pattern.start_position().row + 1,
                start_col: pattern.start_position().column + 1,
                end_line: pattern.end_position().row + 1,
                end_col: pattern.end_position().column + 1,
                parent: None,
                file_total_lines,
                function_lines: None,
            };
            push_symbol(map, symbol_info);
        } else if pattern.kind() == "tuple_pattern" || pattern.kind() == "struct_pattern" {
            extract_identifiers_from_pattern(map, pattern, src, file, file_total_lines);
        }
    }
}

fn extract_identifiers_from_pattern(
    map: &mut RepoMap,
    pattern_node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    if pattern_node.kind() == "identifier" {
        let name = node_text(pattern_node, src).to_string();
        let symbol_info = SymbolInfo {
            name,
            kind: SymbolKind::Variable,
            file: file.to_path_buf(),
            start_line: pattern_node.start_position().row + 1,
            start_col: pattern_node.start_position().column + 1,
            end_line: pattern_node.end_position().row + 1,
            end_col: pattern_node.end_position().column + 1,
            parent: None,
            file_total_lines,
            function_lines: None,
        };
        push_symbol(map, symbol_info);
    } else {
        let mut c = pattern_node.walk();
        if c.goto_first_child() {
            loop {
                extract_identifiers_from_pattern(map, c.node(), src, file, file_total_lines);
                if !c.goto_next_sibling() {
                    break;
                }
            }
            c.goto_parent();
        }
    }
}

fn handle_impl_item(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    let mut parent_name = None;
    if let Some(ty) = node.child_by_field_name("type") {
        parent_name = Some(node_text(ty, src).to_string());
    }
    if let Some(tr) = node.child_by_field_name("trait") {
        parent_name = Some(node_text(tr, src).to_string());
    }
    let impl_name = parent_name.clone().unwrap_or_else(|| "impl".to_string());
    let symbol_info = SymbolInfo {
        name: impl_name.clone(),
        kind: SymbolKind::Impl,
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
    walk_impl_items(
        map,
        &parent_name,
        &impl_name,
        node,
        src,
        file,
        file_total_lines,
    );
}

fn walk_impl_items(
    map: &mut RepoMap,
    parent_name: &Option<String>,
    _impl_name: &str,
    node: Node,
    src: &str,
    file: &Path,
    file_total_lines: usize,
) {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "function_item" {
                let mut has_receiver = false;
                if let Some(params) = child
                    .child_by_field_name("parameters")
                    .or_else(|| child.child_by_field_name("parameter_list"))
                {
                    let mut pc = params.walk();
                    if pc.goto_first_child() {
                        loop {
                            let pchild = pc.node();
                            let k = pchild.kind();
                            if k == "self_parameter" || k == "self" {
                                has_receiver = true;
                                break;
                            }
                            if !pc.goto_next_sibling() {
                                break;
                            }
                        }
                        pc.goto_parent();
                    }
                }
                if let Some(name) = name_from(child, "name", src) {
                    if has_receiver {
                        let symbol_info = SymbolInfo {
                            name,
                            kind: SymbolKind::Method,
                            file: file.to_path_buf(),
                            start_line: child.start_position().row + 1,
                            start_col: child.start_position().column + 1,
                            end_line: child.end_position().row + 1,
                            end_col: child.end_position().column + 1,
                            parent: parent_name.clone(),
                            file_total_lines,
                            function_lines: Some(
                                child.end_position().row - child.start_position().row + 1,
                            ),
                        };
                        push_symbol(map, symbol_info);
                    } else {
                        let symbol_info = SymbolInfo {
                            name,
                            kind: SymbolKind::AssocFn,
                            file: file.to_path_buf(),
                            start_line: child.start_position().row + 1,
                            start_col: child.start_position().column + 1,
                            end_line: child.end_position().row + 1,
                            end_col: child.end_position().column + 1,
                            parent: parent_name.clone(),
                            file_total_lines,
                            function_lines: Some(
                                child.end_position().row - child.start_position().row + 1,
                            ),
                        };
                        push_symbol(map, symbol_info);
                    }
                }
            } else {
                walk_impl_items(
                    map,
                    parent_name,
                    _impl_name,
                    child,
                    src,
                    file,
                    file_total_lines,
                );
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
