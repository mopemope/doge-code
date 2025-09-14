use crate::analysis::{
    CSharpExtractor, GoExtractor, JavaScriptExtractor, LanguageSpecificExtractor, PythonExtractor,
    RustExtractor, TypeScriptExtractor,
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
                extensions: &["ts", "tsx"],
            },
            LanguageConfig {
                language: tree_sitter_javascript::LANGUAGE.into(),
                collector: Box::new(JavaScriptExtractor),
                extensions: &["js", "mjs", "cjs"],
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
