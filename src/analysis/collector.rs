use crate::analysis::RepoMap;
use anyhow::Result;
use regex::Regex;
use std::path::Path;
use tree_sitter::Node;

// Helper functions (kept generic)
pub(super) fn node_text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

pub(super) fn name_from(node: Node, field: &str, src: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(n, src).to_string())
}

/// Extract keywords from comment text
/// This function extracts meaningful words from comments, including both English and Japanese words
pub(super) fn extract_keywords_from_comment(comment: &str) -> Vec<String> {
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
    let word_regex = Regex::new(r"[\w\u3040-\u309F\u30A0-\u30FF\u4E00-\u9FFF_]+").unwrap();

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
        "the",
        "and",
        "for",
        "are",
        "but",
        "not",
        "you",
        "all",
        "can",
        "had",
        "her",
        "was",
        "one",
        "our",
        "out",
        "day",
        "get",
        "has",
        "him",
        "his",
        "how",
        "its",
        "may",
        "new",
        "now",
        "old",
        "see",
        "two",
        "who",
        "boy",
        "did",
        "man",
        "men",
        "put",
        "too",
        "use",
        "any",
        "big",
        "end",
        "far",
        "got",
        "let",
        "lot",
        "run",
        "say",
        "set",
        "she",
        "try",
        "up",
        "way",
        "win",
        "yes",
        "yet",
        "bit",
        "eat",
        "fun",
        "hit",
        "job",
        "key",
        "law",
        "lay",
        "led",
        "log",
        "map",
        "net",
        "oil",
        "pay",
        "pot",
        "raw",
        "red",
        "row",
        "rub",
        "sad",
        "sat",
        "saw",
        "sea",
        "set",
        "shy",
        "sky",
        "sum",
        "sun",
        "tip",
        "top",
        "toy",
        "war",
        "web",
        "wet",
        "win",
        "yes",
        "zip",
        "var",
        "let",
        "const",
        "function",
        "class",
        "struct",
        "enum",
        "trait",
        "impl",
        "mod",
        "pub",
        "fn",
        "async",
        "await",
        "self",
        "this",
        "true",
        "false",
        "null",
        "nil",
        "undefined",
        "void",
        "int",
        "float",
        "bool",
        "char",
        "string",
        "usize",
        "isize",
        "u8",
        "u16",
        "u32",
        "u64",
        "i8",
        "i16",
        "i32",
        "i64",
        "f32",
        "f64",
        "str",
        "bool",
        "vec",
        "map",
        "list",
        "dict",
        "array",
        "slice",
        "tuple",
        "option",
        "result",
        "some",
        "none",
        "ok",
        "err",
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
        "where",
        "type",
        "typeof",
        "instanceof",
        "new",
        "delete",
        "typeof",
        "extends",
        "super",
        "from",
        "import",
        "export",
        "default",
        "as",
        "with",
        "try",
        "catch",
        "finally",
        "throw",
        "throws",
        "assert",
        "static",
        "final",
        "abstract",
        "virtual",
        "override",
        "interface",
        "implements",
        "package",
        "namespace",
        "module",
        "require",
        "define",
        "ifdef",
        "ifndef",
        "endif",
        "pragma",
        "ifdef",
        "ifndef",
        "endif",
        "pragma",
        "ifdef",
        "ifndef",
        "endif",
        "pragma",
        "ifdef",
        "ifndef",
        "endif",
        "pragma",
    ];

    common_keywords.contains(&word.to_lowercase().as_str())
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
