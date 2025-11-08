use super::*;
use crate::analysis::SymbolKind;
use crate::tools::search_repomap::repomap::repomap_filter::filter_and_group_symbols;
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

fn run_response(
    symbols: &[crate::analysis::SymbolInfo],
    args: SearchRepomapArgs,
) -> SearchRepomapResponse {
    filter_and_group_symbols(symbols, args)
}

fn collect_results(
    symbols: &[crate::analysis::SymbolInfo],
    args: SearchRepomapArgs,
) -> Vec<RepomapSearchResult> {
    run_response(symbols, args).results
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

    let results = collect_results(&symbols, args);
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

    let results = collect_results(&symbols, args);
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

    let results = collect_results(&symbols, args);
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

    let results = collect_results(&symbols, args);
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
        keyword_search: Some(vec!["testing".to_string()]),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 1);
    assert_eq!(results[0].symbols[0].name, "test_function");
}

#[test]
fn test_keyword_search_multiple_terms() {
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
        create_test_symbol_with_keywords(
            "math_function",
            SymbolKind::Function,
            "math.rs",
            100,
            Some(20),
            vec!["mathematics".to_string(), "calculation".to_string()],
        ),
    ];

    let args = SearchRepomapArgs {
        keyword_search: Some(vec!["testing".to_string(), "mathematics".to_string()]),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 2);
    // Results should include both test_function and math_function
    let mut found_test = false;
    let mut found_math = false;
    for result in &results {
        for symbol in &result.symbols {
            if symbol.name == "test_function" {
                found_test = true;
            } else if symbol.name == "math_function" {
                found_math = true;
            }
        }
    }
    assert!(found_test);
    assert!(found_math);
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
        create_test_symbol_with_keywords(
            "parse_json",
            SymbolKind::Function,
            "json.rs",
            100,
            Some(20),
            vec!["json".to_string(), "parsing".to_string()],
        ),
    ];

    let args = SearchRepomapArgs {
        name: Some(vec!["calculate".to_string()]),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 1);
    assert_eq!(results[0].symbols[0].name, "calculate_total");
}

#[test]
fn test_name_search_multiple_terms() {
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
        create_test_symbol_with_keywords(
            "parse_json",
            SymbolKind::Function,
            "json.rs",
            100,
            Some(20),
            vec!["json".to_string(), "parsing".to_string()],
        ),
    ];

    let args = SearchRepomapArgs {
        name: Some(vec!["calculate".to_string(), "parse".to_string()]),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 2);
    // Results should include both calculate_total and parse_json
    let mut found_calculate = false;
    let mut found_parse = false;
    for result in &results {
        for symbol in &result.symbols {
            if symbol.name == "calculate_total" {
                found_calculate = true;
            } else if symbol.name == "parse_json" {
                found_parse = true;
            }
        }
    }
    assert!(found_calculate);
    assert!(found_parse);
}

#[test]
fn test_code_doc_field_matching() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file_path = tmp.path().join("example.rs");
    let content = "/// This is a doc line with special_doc_term\nfn example() {\n  // inline comment\n  let x = \"special_code_term\";\n}\n";
    std::fs::write(&file_path, content).unwrap();

    let file_str = file_path.to_str().unwrap();

    let symbols = vec![create_test_symbol(
        "example",
        SymbolKind::Function,
        file_str,
        content.lines().count(),
        Some(4),
    )];

    // doc match
    let args_doc = SearchRepomapArgs {
        fields: Some(vec!["doc".to_string()]),
        keyword_search: Some(vec!["special_doc_term".to_string()]),
        ..Default::default()
    };
    let results_doc = collect_results(&symbols, args_doc);
    assert_eq!(results_doc.len(), 1);
    assert_eq!(results_doc[0].symbols.len(), 1);
    let sym_doc = &results_doc[0].symbols[0];
    assert!(sym_doc.match_score.is_some());
    assert!(!sym_doc.matches.is_empty());
    assert_eq!(sym_doc.matches[0].field, "doc");

    // code match
    let args_code = SearchRepomapArgs {
        fields: Some(vec!["code".to_string()]),
        keyword_search: Some(vec!["special_code_term".to_string()]),
        ..Default::default()
    };
    let results_code = collect_results(&symbols, args_code);
    assert_eq!(results_code.len(), 1);
    let sym_code = &results_code[0].symbols[0];
    assert!(sym_code.match_score.is_some());
    assert!(!sym_code.matches.is_empty());
    assert_eq!(sym_code.matches[0].field, "code");
}

#[test]
fn test_exclude_patterns_skip_files() {
    let symbols = vec![
        create_test_symbol(
            "keep_me",
            SymbolKind::Function,
            "src/keep.rs",
            120,
            Some(12),
        ),
        create_test_symbol(
            "skip_me",
            SymbolKind::Function,
            "src/generated/skip.rs",
            200,
            Some(20),
        ),
    ];

    let args = SearchRepomapArgs {
        exclude_patterns: Some(vec!["generated".to_string()]),
        include_snippets: Some(false),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file, PathBuf::from("src/keep.rs"));
}

#[test]
fn test_language_filters_by_extension() {
    let symbols = vec![
        create_test_symbol("rust_func", SymbolKind::Function, "lib.rs", 80, Some(8)),
        create_test_symbol(
            "python_func",
            SymbolKind::Function,
            "script.py",
            60,
            Some(6),
        ),
    ];

    let args = SearchRepomapArgs {
        language_filters: Some(vec!["rust".to_string()]),
        include_snippets: Some(false),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file, PathBuf::from("lib.rs"));
}

#[test]
fn test_match_score_threshold_filters_symbols() {
    let symbols = vec![
        create_test_symbol_with_keywords(
            "match_symbol",
            SymbolKind::Function,
            "match.rs",
            100,
            Some(10),
            vec!["alpha".to_string()],
        ),
        create_test_symbol_with_keywords(
            "no_match_symbol",
            SymbolKind::Function,
            "match.rs",
            100,
            Some(10),
            vec!["beta".to_string()],
        ),
    ];

    let args = SearchRepomapArgs {
        keyword_search: Some(vec!["alpha".to_string()]),
        match_score_threshold: Some(0.15),
        include_snippets: Some(false),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 1);
    assert_eq!(results[0].symbols[0].name, "match_symbol");
}

#[test]
fn test_max_symbols_per_file_caps_results() {
    let mut sym_a = create_test_symbol("alpha", SymbolKind::Function, "cap.rs", 150, Some(12));
    sym_a.start_line = 5;
    let mut sym_b = create_test_symbol("beta", SymbolKind::Function, "cap.rs", 150, Some(14));
    sym_b.start_line = 30;
    let mut sym_c = create_test_symbol("gamma", SymbolKind::Function, "cap.rs", 150, Some(16));
    sym_c.start_line = 60;

    let symbols = vec![sym_a, sym_b, sym_c];

    let args = SearchRepomapArgs {
        max_symbols_per_file: Some(2),
        include_snippets: Some(false),
        ..Default::default()
    };

    let results = collect_results(&symbols, args);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbols.len(), 2);
    assert_eq!(results[0].symbol_count, 2);
}

#[test]
fn test_compact_density_limits_snippets_and_symbols() {
    let tmp = tempfile::TempDir::new().unwrap();
    let file_path = tmp.path().join("dense.rs");
    let content = (0..20)
        .map(|idx| format!("fn item{}() {{}}", idx))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&file_path, content).unwrap();

    let mut symbols = Vec::new();
    for idx in 0..6 {
        let mut symbol = create_test_symbol(
            &format!("func{}", idx),
            SymbolKind::Function,
            file_path.to_str().unwrap(),
            200,
            Some(4),
        );
        symbol.start_line = idx + 1;
        symbol.end_line = idx + 2;
        symbols.push(symbol);
    }

    let full_response = run_response(
        &symbols,
        SearchRepomapArgs {
            result_density: Some(ResultDensity::Full),
            include_snippets: Some(true),
            ..Default::default()
        },
    );
    assert!(!full_response.results[0].symbols[0].code_snippet.is_empty());

    let compact_response = run_response(
        &symbols,
        SearchRepomapArgs {
            result_density: Some(ResultDensity::Compact),
            include_snippets: Some(true),
            max_symbols_per_file: Some(10),
            snippet_max_chars: Some(2000),
            ..Default::default()
        },
    );
    let file_entry = &compact_response.results[0];
    assert_eq!(file_entry.symbols.len(), 5);
    assert!(
        file_entry
            .symbols
            .iter()
            .all(|symbol| symbol.code_snippet.is_empty())
    );
}

#[test]
fn test_response_budget_limits_and_sets_cursor() {
    let mut symbols = Vec::new();
    for idx in 0..12 {
        symbols.push(create_test_symbol(
            &format!("func{}", idx),
            SymbolKind::Function,
            &format!("budget_{}.rs", idx),
            120,
            Some(6),
        ));
    }

    let response = run_response(
        &symbols,
        SearchRepomapArgs {
            result_density: Some(ResultDensity::Full),
            include_snippets: Some(false),
            response_budget_chars: Some(1500),
            ..Default::default()
        },
    );

    let budget = response.applied_budget.expect("budget summary missing");
    assert!(response.results.len() <= budget.effective_limit);
    assert!(response.next_cursor.is_some());
    assert!(
        response
            .warnings
            .iter()
            .any(|warning| warning.contains("response budget"))
    );
}

#[test]
fn test_cursor_pagination_returns_expected_slice() {
    let mut symbols = Vec::new();
    for idx in 0..8 {
        symbols.push(create_test_symbol(
            &format!("func{}", idx),
            SymbolKind::Function,
            &format!("file_{}.rs", idx),
            80,
            Some(5),
        ));
    }

    let response = run_response(
        &symbols,
        SearchRepomapArgs {
            result_density: Some(ResultDensity::Full),
            include_snippets: Some(false),
            sort_by: Some("file_path".to_string()),
            sort_desc: Some(false),
            limit: Some(8),
            cursor: Some(3),
            page_size: Some(3),
            ..Default::default()
        },
    );

    assert_eq!(response.results.len(), 3);
    assert_eq!(response.next_cursor, Some(6));
    assert_eq!(response.results[0].file, PathBuf::from("file_3.rs"));
}
