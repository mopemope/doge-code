use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tree_sitter::{Node, Parser, Tree};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoMap {
    pub symbols: Vec<SymbolInfo>,
}

pub struct Analyzer {
    root: PathBuf,
    parser: Parser,
}

impl Analyzer {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let mut parser = Parser::new();
        // tree-sitter-rust 0.23 exposes LANGUAGE const convertible to Language
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .context("set rust language")?;
        Ok(Self {
            root: root.into(),
            parser,
        })
    }

    fn parse_file(&mut self, path: &Path) -> Result<Tree> {
        let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let tree = self
            .parser
            .parse(src, None)
            .ok_or_else(|| anyhow::anyhow!("parse returned None"))?;
        Ok(tree)
    }

    pub fn build(&mut self) -> Result<RepoMap> {
        let mut map = RepoMap::default();
        let walker = globwalk::GlobWalkerBuilder::from_patterns(&self.root, &["**/*.rs"]) // rust only for now
            .follow_links(false)
            .case_insensitive(true)
            .build()
            .context("build glob walker")?;
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let p = entry.path().to_path_buf();
            if entry.file_type().is_dir() {
                continue;
            }
            if let Ok(tree) = self.parse_file(&p) {
                let src = fs::read_to_string(&p).unwrap_or_default();
                collect_symbols(&mut map, &tree, &src, &p);
            }
        }
        Ok(map)
    }
}

fn collect_symbols(map: &mut RepoMap, tree: &Tree, src: &str, file: &Path) {
    let root = tree.root_node();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        visit_node(map, node, src, file);
    }
}

fn visit_node(map: &mut RepoMap, node: Node, src: &str, file: &Path) {
    let kind = node.kind();
    if kind == "function_item" {
        if let Some(ident) = node.child_by_field_name("name") {
            let name = ident.utf8_text(src.as_bytes()).unwrap_or("").to_string();
            let line = node.start_position().row + 1;
            map.symbols.push(SymbolInfo {
                name,
                kind: "fn".into(),
                file: file.to_path_buf(),
                line,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_node(map, child, src, file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_rust_functions() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "fn alpha() {{}}\nmod m {{ pub fn beta() {{}} }}").unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let names: Vec<_> = map.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }
}
