use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use std::path::Path;
use tree_sitter::Node;

// ---------------- Rust -----------------
pub fn collect_symbols_rust(map: &mut RepoMap, tree: &tree_sitter::Tree, src: &str, file: &Path) {
    let root = tree.root_node();
    visit_node(map, root, src, file, None);
}

fn node_text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn name_from(node: Node, field: &str, src: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(n, src).to_string())
}

fn push_symbol(
    map: &mut RepoMap,
    kind: SymbolKind,
    name: String,
    node: Node,
    file: &Path,
    parent: Option<String>,
) {
    let start_line = node.start_position().row + 1;
    let start_col = node.start_position().column + 1;
    let end_line = node.end_position().row + 1;
    let end_col = node.end_position().column + 1;
    map.symbols.push(SymbolInfo {
        name,
        kind,
        file: file.to_path_buf(),
        start_line,
        start_col,
        end_line,
        end_col,
        parent,
    });
}

fn visit_node(map: &mut RepoMap, node: Node, src: &str, file: &Path, ctx_impl: Option<String>) {
    match node.kind() {
        "function_item" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Function, name, node, file, None);
            }
        }
        "struct_item" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Struct, name, node, file, None);
            }
        }
        "enum_item" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Enum, name, node, file, None);
            }
        }
        "trait_item" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Trait, name, node, file, None);
            }
        }
        "mod_item" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Mod, name, node, file, None);
            }
        }
        "let_declaration" => {
            // Extract variable name from pattern
            if let Some(pattern) = node.child_by_field_name("pattern") {
                // Simple case: let x = ...;
                if pattern.kind() == "identifier" {
                    let name = node_text(pattern, src).to_string();
                    push_symbol(map, SymbolKind::Variable, name, pattern, file, None);
                } else if pattern.kind() == "tuple_pattern" || pattern.kind() == "struct_pattern" {
                    // Complex patterns like let (a, b) = ...; or let Point { x, y } = ...;
                    // For simplicity, we can try to extract identifiers from these patterns
                    // This is a basic implementation, can be expanded for more complex cases
                    fn extract_identifiers_from_pattern(
                        map: &mut RepoMap,
                        pattern_node: Node,
                        src: &str,
                        file: &Path,
                    ) {
                        if pattern_node.kind() == "identifier" {
                            let name = node_text(pattern_node, src).to_string();
                            push_symbol(map, SymbolKind::Variable, name, pattern_node, file, None);
                        } else {
                            let mut c = pattern_node.walk();
                            for child in pattern_node.children(&mut c) {
                                extract_identifiers_from_pattern(map, child, src, file);
                            }
                        }
                    }
                    extract_identifiers_from_pattern(map, pattern, src, file);
                }
                // Other patterns like array_pattern etc. can be added similarly if needed
            }
        }
        "impl_item" => {
            let mut parent_name = None;
            if let Some(ty) = node.child_by_field_name("type") {
                parent_name = Some(node_text(ty, src).to_string());
            }
            if let Some(tr) = node.child_by_field_name("trait") {
                parent_name = Some(node_text(tr, src).to_string());
            }
            let impl_name = parent_name.clone().unwrap_or_else(|| "impl".to_string());
            // Record the impl itself
            push_symbol(map, SymbolKind::Impl, impl_name.clone(), node, file, None);
            // Walk items inside impl (deep scan to catch declaration_list/function_item)
            fn walk_impl_items(
                map: &mut RepoMap,
                parent_name: &Option<String>,
                _impl_name: &str,
                node: Node,
                src: &str,
                file: &Path,
            ) {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "function_item" {
                        // Distinguish method vs associated function by presence of receiver
                        let mut has_receiver = false;
                        if let Some(params) = child
                            .child_by_field_name("parameters")
                            .or_else(|| child.child_by_field_name("parameter_list"))
                        {
                            let mut pc = params.walk();
                            for pchild in params.children(&mut pc) {
                                let k = pchild.kind();
                                if k == "self_parameter" || k == "self" {
                                    has_receiver = true;
                                    break;
                                }
                            }
                        }
                        if let Some(name) = name_from(child, "name", src) {
                            if has_receiver {
                                push_symbol(
                                    map,
                                    SymbolKind::Method,
                                    name,
                                    child,
                                    file,
                                    parent_name.clone(),
                                );
                            } else {
                                push_symbol(
                                    map,
                                    SymbolKind::AssocFn,
                                    name,
                                    child,
                                    file,
                                    parent_name.clone(),
                                );
                            }
                        }
                    } else {
                        // Recurse deeper (e.g., declaration_list)
                        walk_impl_items(map, parent_name, _impl_name, child, src, file);
                    }
                }
            }
            walk_impl_items(&mut *map, &parent_name, &impl_name, node, src, file);
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_node(map, child, src, file, ctx_impl.clone());
    }
}

// ---------------- TypeScript/JavaScript -----------------
pub fn collect_symbols_ts(map: &mut RepoMap, tree: &tree_sitter::Tree, src: &str, file: &Path) {
    collect_ts_js(map, tree, src, file, true);
}

pub fn collect_symbols_js(map: &mut RepoMap, tree: &tree_sitter::Tree, src: &str, file: &Path) {
    collect_ts_js(map, tree, src, file, false);
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
    for node in root.children(&mut cursor) {
        visit_ts_js(map, node, src, file, None);
    }
}

fn visit_ts_js(map: &mut RepoMap, node: Node, src: &str, file: &Path, class_ctx: Option<String>) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Function, name, node, file, None);
            }
        }
        "class_declaration" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Struct, name.clone(), node, file, None);
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    visit_ts_js(map, child, src, file, Some(name.clone()));
                }
                return;
            }
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, src).to_string();
                push_symbol(map, SymbolKind::Method, name, node, file, class_ctx.clone());
            }
        }
        "enum_declaration" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Enum, name, node, file, None);
            }
        }
        "interface_declaration" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Trait, name, node, file, None);
            }
        }
        // Handle variable declarations (var, let, const)
        "lexical_declaration" | "variable_declaration" => {
            // These nodes contain a list of variable declarators
            let mut c = node.walk();
            for child in node.children(&mut c) {
                if child.kind() == "variable_declarator"
                    && let Some(id_node) = child.child_by_field_name("name")
                {
                    let name = node_text(id_node, src).to_string();
                    push_symbol(map, SymbolKind::Variable, name, id_node, file, None);
                }
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        visit_ts_js(map, child, src, file, class_ctx.clone());
    }
}

// ---------------- Python -----------------
pub fn collect_symbols_py(map: &mut RepoMap, tree: &tree_sitter::Tree, src: &str, file: &Path) {
    let root = tree.root_node();
    visit_py(map, root, src, file, None);
}

fn visit_py(map: &mut RepoMap, node: Node, src: &str, file: &Path, class_ctx: Option<String>) {
    match node.kind() {
        "function_definition" => {
            if let Some(name) = name_from(node, "name", src) {
                let is_method = class_ctx.is_some() && first_param_is_self_or_cls(node, src);
                let kind = if is_method {
                    SymbolKind::Method
                } else if class_ctx.is_some() {
                    SymbolKind::AssocFn
                } else {
                    SymbolKind::Function
                };
                push_symbol(map, kind, name, node, file, class_ctx.clone());
            }
        }
        "class_definition" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Struct, name.clone(), node, file, None);
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    visit_py(map, child, src, file, Some(name.clone()));
                }
                return;
            }
        }
        // Handle simple assignments (var = value)
        "assignment" => {
            // Left hand side is the target, right hand side is the value
            if let Some(lhs) = node.child_by_field_name("left") {
                // Simple case: identifier = ...
                if lhs.kind() == "identifier" {
                    let name = node_text(lhs, src).to_string();
                    push_symbol(map, SymbolKind::Variable, name, lhs, file, None);
                } else if lhs.kind() == "pattern_list" || lhs.kind() == "tuple_pattern" {
                    // Multiple assignments like a, b = ...
                    // Extract identifiers from the left-hand side pattern
                    fn extract_identifiers_from_py_lhs(
                        map: &mut RepoMap,
                        lhs_node: Node,
                        src: &str,
                        file: &Path,
                    ) {
                        if lhs_node.kind() == "identifier" {
                            let name = node_text(lhs_node, src).to_string();
                            push_symbol(map, SymbolKind::Variable, name, lhs_node, file, None);
                        } else {
                            let mut c = lhs_node.walk();
                            for child in lhs_node.children(&mut c) {
                                extract_identifiers_from_py_lhs(map, child, src, file);
                            }
                        }
                    }
                    extract_identifiers_from_py_lhs(map, lhs, src, file);
                }
                // Other patterns like list/dict unpacking can be handled if needed
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        visit_py(map, child, src, file, class_ctx.clone());
    }
}

fn first_param_is_self_or_cls(fn_node: Node, src: &str) -> bool {
    if let Some(params) = fn_node.child_by_field_name("parameters") {
        let mut c = params.walk();
        for child in params.children(&mut c) {
            if child.kind() == "identifier" {
                let name = node_text(child, src);
                return name == "self" || name == "cls";
            }
        }
    }
    false
}
