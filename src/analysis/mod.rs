pub mod analyzer;
pub mod c_collector;
pub mod cache;
pub mod collector;
pub mod cpp_collector;
pub mod csharp_collector;
pub mod database;
pub mod file_finder;
pub mod go_collector;
pub mod hash;
pub mod language_config;
pub mod md_collector;
pub mod parser;
pub mod python_collector;
pub mod rust_collector;
pub mod symbol;
pub mod symbol_identity;
pub mod symbol_utils;
pub mod tests;
pub mod ts_js_collector;

pub use analyzer::Analyzer;
pub use c_collector::CExtractor;
pub use cache::{RepomapCache, RepomapStore, ensure_repomap_ready};
pub use collector::LanguageSpecificExtractor;
pub use cpp_collector::CppExtractor;
pub use csharp_collector::CSharpExtractor;
pub use database::connection::{connect_database, get_default_db_path};
pub use go_collector::GoExtractor;
pub use hash::{HashDiff, calculate_file_hashes};
pub use md_collector::MarkdownExtractor;
pub use python_collector::PythonExtractor;
pub use rust_collector::RustExtractor;
pub use symbol::{RelationType, RepoMap, SymbolInfo, SymbolKind, SymbolRelation};
pub use symbol_identity::{
    ContentFingerprint, SourceSpan, SymbolId, SymbolIdentityIndex, fingerprint_symbol,
    fingerprint_symbol_source, is_targetable_kind, normalize_relative_path, source_span,
    symbol_source,
};
pub use symbol_utils::{SymbolSpan, find_enclosing_symbol, index_symbols_by_file, list_symbols};
pub use ts_js_collector::{JavaScriptExtractor, TypeScriptExtractor};

pub mod context;
pub use context::ContextManager;

pub mod loop_detector;
pub mod task_sentinel;
pub use loop_detector::LoopDetector;
pub use task_sentinel::TaskSentinel;
