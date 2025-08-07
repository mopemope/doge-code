use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tree_sitter::{Language, Node, Parser, Tree};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Method,
    AssocFn,
    Mod,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Method => "method",
            SymbolKind::AssocFn => "assoc_fn",
            SymbolKind::Mod => "mod",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoMap {
    pub symbols: Vec<SymbolInfo>,
}

pub struct Analyzer {
    root: PathBuf,
    parser: Parser,
    lang: Language,
}

impl Analyzer {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let mut parser = Parser::new();
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).context("set rust language")?;
        Ok(Self {
            root: root.into(),
            parser,
            lang,
        })
    }

    fn parse_file(&mut self, path: &Path) -> Result<(Tree, String)> {
        let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        // Switch language by extension
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let lang: Language = match ext {
            "rs" => tree_sitter_rust::LANGUAGE.into(),
            "ts" | "tsx" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "js" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
            "py" => tree_sitter_python::LANGUAGE.into(),
            _ => tree_sitter_rust::LANGUAGE.into(),
        };
        if self.lang != lang {
            self.parser.set_language(&lang).context("set language")?;
            self.lang = lang;
        }
        let tree = self
            .parser
            .parse(&src, None)
            .ok_or_else(|| anyhow::anyhow!("parse returned None"))?;
        Ok((tree, src))
    }

    pub fn build(&mut self) -> Result<RepoMap> {
        let mut map = RepoMap::default();
        let walker = globwalk::GlobWalkerBuilder::from_patterns(
            &self.root,
            &["**/*.rs", "**/*.ts", "**/*.tsx", "**/*.js", "**/*.py"],
        )
        .follow_links(false)
        .case_insensitive(true)
        .build()
        .context("build glob walker")?;
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().is_dir() {
                continue;
            }
            let p = entry.path().to_path_buf();
            if let Ok((tree, src)) = self.parse_file(&p) {
                let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                match ext {
                    "rs" => collect_symbols_rust(&mut map, &tree, &src, &p),
                    "ts" | "tsx" => collect_symbols_ts(&mut map, &tree, &src, &p),
                    "js" | "mjs" | "cjs" => collect_symbols_js(&mut map, &tree, &src, &p),
                    "py" => collect_symbols_py(&mut map, &tree, &src, &p),
                    _ => collect_symbols_rust(&mut map, &tree, &src, &p),
                }
            }
        }
        Ok(map)
    }
}

// ---------------- Rust -----------------
fn collect_symbols_rust(map: &mut RepoMap, tree: &Tree, src: &str, file: &Path) {
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
fn collect_symbols_ts(map: &mut RepoMap, tree: &Tree, src: &str, file: &Path) {
    collect_ts_js(map, tree, src, file, true);
}

fn collect_symbols_js(map: &mut RepoMap, tree: &Tree, src: &str, file: &Path) {
    collect_ts_js(map, tree, src, file, false);
}

fn collect_ts_js(map: &mut RepoMap, tree: &Tree, src: &str, file: &Path, _is_ts: bool) {
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
fn collect_symbols_py(map: &mut RepoMap, tree: &Tree, src: &str, file: &Path) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_ts_symbols_minimal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.ts");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            "
function foo() {{}}
interface I {{ x: number }}
class C {{ bar() {{}} }}
enum E {{ A, B }}
"
        )
        .unwrap();
        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&("fn", "foo")));
        assert!(names.contains(&("trait", "I")));
        assert!(names.contains(&("struct", "C")));
        assert!(names.contains(&("method", "bar")));
        assert!(names.contains(&("enum", "E")));
    }

    #[test]
    fn parse_js_symbols_minimal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.js");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            "
function foo() {{}}
class C {{ bar() {{}} }}
"
        )
        .unwrap();
        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&("fn", "foo")));
        assert!(names.contains(&("struct", "C")));
        assert!(names.contains(&("method", "bar")));
    }

    #[test]
    fn parse_py_symbols_minimal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.py");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            "
class C:
    def bar(self):
        pass

def foo():
    pass
"
        )
        .unwrap();
        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&("struct", "C")));
        assert!(names.contains(&("method", "bar")));
        assert!(names.contains(&("fn", "foo")));
    }

    // keep Rust test last; avoid duplicate imports
    #[test]
    fn parse_rust_symbols_various() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        let mut f = fs::File::create(&path).unwrap();
        // Use write! with escaped braces in raw string to avoid format! placeholders
        write!(
            f,
            "
fn alpha() {{}}
mod m {{ pub fn beta() {{}} }}
struct S {{ x: i32 }}
enum E {{ A, B }}
trait T {{ fn t(&self); }}
impl S {{ fn new() -> Self {{ S {{ x: 0 }} }} fn method(&self) {{}} }}
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let by_kind = |k: SymbolKind| -> Vec<String> {
            map.symbols
                .iter()
                .filter(|s| s.kind == k)
                .map(|s| s.name.clone())
                .collect()
        };
        assert!(by_kind(SymbolKind::Function).contains(&"alpha".to_string()));
        assert!(by_kind(SymbolKind::Struct).contains(&"S".to_string()));
        assert!(by_kind(SymbolKind::Enum).contains(&"E".to_string()));
        assert!(by_kind(SymbolKind::Trait).contains(&"T".to_string()));
        // impl symbol present and methods/assoc fns captured
        assert!(map.symbols.iter().any(|s| s.kind == SymbolKind::Impl));
        assert!(by_kind(SymbolKind::AssocFn).contains(&"new".to_string()));
        assert!(by_kind(SymbolKind::Method).contains(&"method".to_string()));
    }
}
