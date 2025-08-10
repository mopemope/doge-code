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
    let end_line = node.end_position().row + 1;
    map.symbols.push(SymbolInfo {
        name,
        kind,
        file: file.to_path_buf(),
        start_line,
        end_line,
        parent,
    });
}

fn visit_node(map: &mut RepoMap, node: Node, src: &str, file: &Path, ctx_impl: Option<String>) {
    match node.kind() {
        "function_item" => {
            if let Some(name) = name_from(node, "name", src) {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analysis] fn {} @{}:{}",
                    name,
                    file.display(),
                    node.start_position().row + 1
                );
                push_symbol(map, SymbolKind::Function, name, node, file, None);
            }
        }
        "struct_item" => {
            if let Some(name) = name_from(node, "name", src) {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analysis] struct {} @{}:{}",
                    name,
                    file.display(),
                    node.start_position().row + 1
                );
                push_symbol(map, SymbolKind::Struct, name, node, file, None);
            }
        }
        "enum_item" => {
            if let Some(name) = name_from(node, "name", src) {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analysis] enum {} @{}:{}",
                    name,
                    file.display(),
                    node.start_position().row + 1
                );
                push_symbol(map, SymbolKind::Enum, name, node, file, None);
            }
        }
        "trait_item" => {
            if let Some(name) = name_from(node, "name", src) {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analysis] trait {} @{}:{}",
                    name,
                    file.display(),
                    node.start_position().row + 1
                );
                push_symbol(map, SymbolKind::Trait, name, node, file, None);
            }
        }
        "mod_item" => {
            if let Some(name) = name_from(node, "name", src) {
                #[cfg(debug_assertions)]
                eprintln!(
                    "[analysis] mod {} @{}:{}",
                    name,
                    file.display(),
                    node.start_position().row + 1
                );
                push_symbol(map, SymbolKind::Mod, name, node, file, None);
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
            #[cfg(debug_assertions)]
            eprintln!(
                "[analysis] impl {} @{}:{}",
                impl_name,
                file.display(),
                node.start_position().row + 1
            );
            // Record the impl itself
            push_symbol(map, SymbolKind::Impl, impl_name.clone(), node, file, None);
            // Walk items inside impl (deep scan to catch declaration_list/function_item)
            fn walk_impl_items(
                map: &mut RepoMap,
                parent_name: &Option<String>,
                impl_name: &str,
                node: Node,
                src: &str,
                file: &Path,
            ) {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "[analysis] impl-desc child kind={} @line {}",
                        child.kind(),
                        child.start_position().row + 1
                    );
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
                            #[cfg(debug_assertions)]
                            eprintln!(
                                "[analysis] impl fn {name} (method={has_receiver}) parent={impl_name}"
                            );
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
                        walk_impl_items(map, parent_name, impl_name, child, src, file);
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
