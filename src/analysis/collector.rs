use crate::analysis::RepoMap;
use anyhow::Result;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::Node;

static WORD_REGEX: OnceLock<Regex> = OnceLock::new();

// Helper functions (kept generic)
pub(super) fn node_text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

pub(super) fn name_from(node: Node, field: &str, src: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(n, src).to_string())
}

/// Extract keywords from comment text
/// This function extracts meaningful words from comments, including both English and Japanese words.
pub fn extract_keywords_from_comment(comment: &str) -> Vec<String> {
    let mut keywords = Vec::new();

    // Remove comment markers (//, /*, */, #, etc.)
    let clean_comment = comment
        .trim()
        .strip_prefix("//")
        .or_else(|| comment.strip_prefix("/*"))
        .or_else(|| comment.strip_prefix("#"))
        .unwrap_or(comment)
        .trim_end()
        .trim_end_matches("*/");

    // Simple regex to extract words (alphanumeric + Japanese characters)
    // This pattern matches sequences of:
    // - ASCII letters and numbers
    // - Japanese hiragana, katakana, and kanji characters
    // - Underscores
    let word_regex = WORD_REGEX.get_or_init(|| {
        Regex::new(r"[\w\u3040-\u309F\u30A0-\u30FF\u4E00-\u9FFF_]+").expect("valid word regex")
    });

    for mat in word_regex.find_iter(clean_comment) {
        let word = mat.as_str().trim();
        // Filter out very short words and common programming keywords
        if word.len() > 1 && !is_common_programming_keyword(word) {
            keywords.push(word.to_string());
        }
    }

    keywords
}

/// Check if a word is a common programming keyword that should be filtered out
fn is_common_programming_keyword(word: &str) -> bool {
    let common_keywords = [
        // English Stop Words (Pronouns, Prepositions, Conjunctions)
        "the",
        "and",
        "but",
        "not",
        "you",
        "all",
        "any",
        "her",
        "him",
        "his",
        "how",
        "its",
        "our",
        "out",
        "she",
        "that",
        "this",
        "to",
        "was",
        "what",
        "when",
        "where",
        "who",
        "why",
        "with",
        // Common Programming Keywords & Types
        "if",
        "else",
        "match",
        "loop",
        "while",
        "for",
        "in",
        "break",
        "continue",
        "return",
        "yield",
        "switch",
        "case",
        "default",
        "do",
        "goto",
        "try",
        "catch",
        "finally",
        "throw",
        "throws",
        "raise",
        "async",
        "await",
        "future",
        "promise",
        "import",
        "export",
        "from",
        "as",
        "package",
        "module",
        "namespace",
        "use",
        "crate",
        "class",
        "struct",
        "enum",
        "trait",
        "interface",
        "impl",
        "type",
        "typedef",
        "union",
        "fn",
        "fun",
        "func",
        "function",
        "def",
        "method",
        "procedure",
        "var",
        "let",
        "const",
        "static",
        "final",
        "mut",
        "public",
        "private",
        "protected",
        "internal",
        "pub",
        "true",
        "false",
        "null",
        "nil",
        "none",
        "undefined",
        "void",
        "new",
        "delete",
        "typeof",
        "instanceof",
        "extends",
        "super",
        "abstract",
        "virtual",
        "override",
        "self",
        "this",
        // Primitive Types (generic)
        "int",
        "float",
        "double",
        "bool",
        "boolean",
        "char",
        "string",
        "str",
        "byte",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "f32",
        "f64",
        // Preprocessor
        "define",
        "undef",
        "ifdef",
        "ifndef",
        "endif",
        "pragma",
        "include",
    ];

    common_keywords
        .iter()
        .any(|&kw| kw.eq_ignore_ascii_case(word))
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
