use crate::analysis::{RepoMap, SymbolInfo, SymbolKind};
use anyhow::Result;
use std::path::Path;
use tree_sitter::Node;

// Helper functions (kept generic)
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

// Trait for language-specific symbol extraction
pub trait LanguageSpecificExtractor: Send + Sync {
    fn extract_symbols(
        &self,
        map: &mut RepoMap,
        tree: &tree_sitter::Tree,
        src: &str,
        file: &Path,
    ) -> Result<()>;
}

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
        visit_rust_node(map, root, src, file, None);
        Ok(())
    }
}

fn visit_rust_node(
    map: &mut RepoMap,
    node: Node,
    src: &str,
    file: &Path,
    ctx_impl: Option<String>,
) {
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
            if let Some(pattern) = node.child_by_field_name("pattern") {
                if pattern.kind() == "identifier" {
                    let name = node_text(pattern, src).to_string();
                    push_symbol(map, SymbolKind::Variable, name, pattern, file, None);
                } else if pattern.kind() == "tuple_pattern" || pattern.kind() == "struct_pattern" {
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
                            if c.goto_first_child() {
                                loop {
                                    extract_identifiers_from_pattern(map, c.node(), src, file);
                                    if !c.goto_next_sibling() {
                                        break;
                                    }
                                }
                                c.goto_parent();
                            }
                        }
                    }
                    extract_identifiers_from_pattern(map, pattern, src, file);
                }
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
            push_symbol(map, SymbolKind::Impl, impl_name.clone(), node, file, None);
            fn walk_impl_items(
                map: &mut RepoMap,
                parent_name: &Option<String>,
                _impl_name: &str,
                node: Node,
                src: &str,
                file: &Path,
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
                            walk_impl_items(map, parent_name, _impl_name, child, src, file);
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                    cursor.goto_parent();
                }
            }
            walk_impl_items(&mut *map, &parent_name, &impl_name, node, src, file);
        }
        _ => {}
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            visit_rust_node(map, cursor.node(), src, file, ctx_impl.clone());
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

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
        "lexical_declaration" | "variable_declaration" => {
            let mut c = node.walk();
            if c.goto_first_child() {
                loop {
                    let child = c.node();
                    if child.kind() == "variable_declarator"
                        && let Some(id_node) = child.child_by_field_name("name")
                    {
                        let name = node_text(id_node, src).to_string();
                        push_symbol(map, SymbolKind::Variable, name, id_node, file, None);
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
                c.goto_parent();
            }
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
        "assignment" => {
            if let Some(lhs) = node.child_by_field_name("left") {
                if lhs.kind() == "identifier" {
                    let name = node_text(lhs, src).to_string();
                    push_symbol(map, SymbolKind::Variable, name, lhs, file, None);
                } else if lhs.kind() == "pattern_list" || lhs.kind() == "tuple_pattern" {
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
                            if c.goto_first_child() {
                                loop {
                                    extract_identifiers_from_py_lhs(map, c.node(), src, file);
                                    if !c.goto_next_sibling() {
                                        break;
                                    }
                                }
                                c.goto_parent();
                            }
                        }
                    }
                    extract_identifiers_from_py_lhs(map, lhs, src, file);
                }
            }
        }
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
    match node.kind() {
        "function_declaration" => {
            if let Some(name) = name_from(node, "name", src) {
                push_symbol(map, SymbolKind::Function, name, node, file, None);
            }
        }
        "method_declaration" => {
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
                push_symbol(map, SymbolKind::Method, name, node, file, receiver_type);
            }
        }
        "type_declaration" => {
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
                        push_symbol(map, kind, name.clone(), child, file, None);
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
                c.goto_parent();
            }
        }
        "const_declaration" | "var_declaration" => {
            let mut c = node.walk();
            if c.goto_first_child() {
                loop {
                    let child = c.node();
                    if (child.kind() == "const_spec" || child.kind() == "var_spec")
                        && let Some(name_node) = child.child_by_field_name("name")
                    {
                        let name = node_text(name_node, src).to_string();
                        push_symbol(map, SymbolKind::Variable, name, name_node, file, None);
                    }
                    if !c.goto_next_sibling() {
                        break;
                    }
                }
                c.goto_parent();
            }
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
