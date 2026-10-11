use crate::analysis::{
    CExtractor, CSharpExtractor, CppExtractor, GoExtractor, JavaScriptExtractor,
    LanguageSpecificExtractor, MarkdownExtractor, PythonExtractor, RustExtractor,
    TypeScriptExtractor,
};
use std::{collections::HashMap, sync::OnceLock};
use tree_sitter::Language;

pub struct LanguageConfig {
    pub language: Language,
    pub collector: Box<dyn LanguageSpecificExtractor>,
    pub extensions: &'static [&'static str],
}

pub fn language_configs() -> &'static [LanguageConfig] {
    static CONFIGS: OnceLock<Vec<LanguageConfig>> = OnceLock::new();
    CONFIGS.get_or_init(|| {
        vec![
            LanguageConfig {
                language: tree_sitter_rust::LANGUAGE.into(),
                collector: Box::new(RustExtractor),
                extensions: &["rs"],
            },
            LanguageConfig {
                language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                collector: Box::new(TypeScriptExtractor),
                extensions: &["ts"],
            },
            LanguageConfig {
                language: tree_sitter_typescript::LANGUAGE_TSX.into(),
                collector: Box::new(TypeScriptExtractor),
                extensions: &["tsx"],
            },
            LanguageConfig {
                language: tree_sitter_javascript::LANGUAGE.into(),
                collector: Box::new(JavaScriptExtractor),
                extensions: &["js", "jsx", "mjs", "cjs"],
            },
            LanguageConfig {
                language: tree_sitter_python::LANGUAGE.into(),
                collector: Box::new(PythonExtractor),
                extensions: &["py"],
            },
            LanguageConfig {
                language: tree_sitter_go::LANGUAGE.into(),
                collector: Box::new(GoExtractor),
                extensions: &["go"],
            },
            LanguageConfig {
                language: tree_sitter_c_sharp::LANGUAGE.into(),
                collector: Box::new(CSharpExtractor),
                extensions: &["cs"],
            },
            LanguageConfig {
                language: tree_sitter_c::LANGUAGE.into(),
                collector: Box::new(CExtractor),
                extensions: &["c", "h"],
            },
            LanguageConfig {
                language: tree_sitter_cpp::LANGUAGE.into(),
                collector: Box::new(CppExtractor),
                extensions: &["cpp", "cxx", "cc", "hpp", "hxx", "hh"],
            },
            LanguageConfig {
                language: tree_sitter_md::LANGUAGE.into(),
                collector: Box::new(MarkdownExtractor),
                extensions: &["md"],
            },
        ]
    })
}

pub fn extension_map() -> &'static HashMap<&'static str, &'static LanguageConfig> {
    static EXT_MAP: OnceLock<HashMap<&'static str, &'static LanguageConfig>> = OnceLock::new();
    EXT_MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for config in language_configs() {
            for &ext in config.extensions {
                map.insert(ext, config);
            }
        }
        map
    })
}

#[cfg(test)]
mod runtime_compatibility_tests {
    use super::*;
    #[test]
    fn every_registered_grammar_parses_with_current_runtime() {
        for (&extension, config) in extension_map() {
            let source = match extension {
                "rs" => "fn main() {}",
                "ts" => "const value: number = 1;",
                "tsx" => "export function App() { return <div>Hello</div>; }",
                "js" | "mjs" | "cjs" => "function main() {}",
                "jsx" => "export function App() { return <div>Hello</div>; }",
                "py" => "def main():\n    pass\n",
                "go" => "package main\nfunc main() {}",
                "cs" => "class Main {}",
                "c" => "int main(void) { return 0; }",
                "cpp" | "cxx" | "cc" | "hpp" | "hxx" | "hh" => "int main() { return 0; }",
                "h" => "int main(void);",
                "md" => "# Heading\n\nBody.\n",
                extension => panic!("uncovered grammar: {extension}"),
            };
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&config.language)
                .expect("compatible grammar ABI");
            let tree = parser.parse(source, None).expect("parsed source");
            assert!(!tree.root_node().has_error(), "{extension}");
        }
    }
}
