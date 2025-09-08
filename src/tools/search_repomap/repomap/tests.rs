use super::*;
use crate::analysis::SymbolKind;
use std::path::PathBuf;

fn create_test_symbol(
    name: &str,
    kind: SymbolKind,
    file: &str,
    file_total_lines: usize,
    function_lines: Option<usize>,
) -> crate::analysis::SymbolInfo {
    crate::analysis::SymbolInfo {
        name: name.to_string(),
        kind,
        file: PathBuf::from(file),
        start_line: 1,
        start_col: 1,
        end_line: function_lines.map(|l| l + 1).unwrap_or(10),
        end_col: 10,
        parent: None,
        file_total_lines,
        function_lines,
        keywords: vec![],
    }
}

fn create_test_symbol_with_keywords(
    name: &str,
    kind: SymbolKind,
    file: &str,
    file_total_lines: usize,
    function_lines: Option<usize>,
    keywords: Vec<String>,
) -> crate::analysis::SymbolInfo {
    crate::analysis::SymbolInfo {
        name: name.to_string(),
        kind,
        file: PathBuf::from(file),
        start_line: 1,
        start_col: 0,
        end_line: function_lines.map(|l| l + 1).unwrap_or(10),
        end_col: 1,
        parent: None,
        file_total_lines,
        function_lines,
        keywords,
    }
}

#[test]
fn test_filter_by_file_lines() {
    let symbols = vec![
        create_test_symbol("func1", SymbolKind::Function, "small.rs", 50, Some(10)),
        create_test_symbol("func2", SymbolKind::Function, "large.rs", 600, Some(20)),
    ];

    let args = SearchRepomapArgs {
        min_file_lines: Some(500),
        ..Default::default()
    };

    let results = filter_and_group_symbols(symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file, PathBuf::from("large.rs"));
    assert_eq!(results[0].file_total_lines, 600);
}

#[test]
fn test_filter_by_function_lines() {
    let symbols = vec![
        create_test_symbol("small_func", SymbolKind::Function, "test.rs", 200, Some(50)),
        create_test_symbol(
            "large_func",
            SymbolKind::Function,
            "test.rs",
            200,
            Some(150),
        ),
    ];

    let args = SearchRepomapArgs {
        min_function_lines: Some(100),
        ..Default::default()
    };

    let results = filter_and_group_symbols(symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 1);
    assert_eq!(results[0].symbols[0].name, "large_func");
}

#[test]
fn test_filter_by_symbol_kind() {
    let symbols = vec![
        create_test_symbol("my_func", SymbolKind::Function, "test.rs", 100, Some(10)),
        create_test_symbol("MyStruct", SymbolKind::Struct, "test.rs", 100, None),
    ];

    let args = SearchRepomapArgs {
        symbol_kinds: Some(vec!["fn".to_string()]),
        ..Default::default()
    };

    let results = filter_and_group_symbols(symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 1);
    assert_eq!(results[0].symbols[0].kind, "fn");
}

#[test]
fn test_sort_by_file_lines() {
    let symbols = vec![
        create_test_symbol("func1", SymbolKind::Function, "small.rs", 100, Some(10)),
        create_test_symbol("func2", SymbolKind::Function, "large.rs", 500, Some(20)),
        create_test_symbol("func3", SymbolKind::Function, "medium.rs", 300, Some(15)),
    ];

    let args = SearchRepomapArgs {
        sort_by: Some("file_lines".to_string()),
        sort_desc: Some(true),
        ..Default::default()
    };

    let results = filter_and_group_symbols(symbols, args);
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].file_total_lines, 500); // largest first
    assert_eq!(results[1].file_total_lines, 300);
    assert_eq!(results[2].file_total_lines, 100);
}

#[test]
fn test_keyword_search() {
    let symbols = vec![
        create_test_symbol_with_keywords(
            "test_function",
            SymbolKind::Function,
            "test.rs",
            100,
            Some(10),
            vec!["testing".to_string(), "functionality".to_string()],
        ),
        create_test_symbol_with_keywords(
            "other_function",
            SymbolKind::Function,
            "test.rs",
            100,
            Some(15),
            vec!["other".to_string(), "utility".to_string()],
        ),
    ];

    let args = SearchRepomapArgs {
        keyword_search: Some("testing".to_string()),
        ..Default::default()
    };

    let results = filter_and_group_symbols(symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 1);
    assert_eq!(results[0].symbols[0].name, "test_function");
}

#[test]
fn test_name_search() {
    let symbols = vec![
        create_test_symbol_with_keywords(
            "calculate_total",
            SymbolKind::Function,
            "math.rs",
            100,
            Some(10),
            vec!["math".to_string(), "calculation".to_string()],
        ),
        create_test_symbol_with_keywords(
            "format_string",
            SymbolKind::Function,
            "string.rs",
            100,
            Some(15),
            vec!["string".to_string(), "format".to_string()],
        ),
    ];

    let args = SearchRepomapArgs {
        name: Some("calculate".to_string()),
        ..Default::default()
    };

    let results = filter_and_group_symbols(symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 1);
    assert_eq!(results[0].symbols[0].name, "calculate_total");
}
