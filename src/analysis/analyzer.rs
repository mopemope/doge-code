use crate::analysis::RepoMap;
use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tree_sitter::{Language, Parser, Tree};

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
                    "rs" => {
                        crate::analysis::collector::collect_symbols_rust(&mut map, &tree, &src, &p)
                    }
                    "ts" | "tsx" => {
                        crate::analysis::collector::collect_symbols_ts(&mut map, &tree, &src, &p)
                    }
                    "js" | "mjs" | "cjs" => {
                        crate::analysis::collector::collect_symbols_js(&mut map, &tree, &src, &p)
                    }
                    "py" => {
                        crate::analysis::collector::collect_symbols_py(&mut map, &tree, &src, &p)
                    }
                    _ => {
                        crate::analysis::collector::collect_symbols_rust(&mut map, &tree, &src, &p)
                    }
                }
            }
        }
        Ok(map)
    }
}
