use std::collections::HashMap;
use std::path::PathBuf;
use std::{fs, str};

use super::{MatchSpan, RepomapSearchResult, SearchRepomapArgs, SymbolSearchResult};
use crate::analysis::SymbolInfo;

pub(super) fn filter_and_group_symbols(
    symbols: &[SymbolInfo],
    args: SearchRepomapArgs,
) -> Vec<RepomapSearchResult> {
    // Group symbols by file
    let mut file_groups: HashMap<PathBuf, Vec<&SymbolInfo>> = HashMap::new();
    for symbol in symbols {
        file_groups
            .entry(symbol.file.clone())
            .or_default()
            .push(symbol);
    }

    let mut results = Vec::new();

    for (file_path, file_symbols) in file_groups {
        // Get file total lines from any symbol in the file (they should all have the same value)
        let file_total_lines = file_symbols
            .first()
            .map(|s| s.file_total_lines)
            .unwrap_or(0);

        // Apply file-level filters
        if let Some(min_lines) = args.min_file_lines
            && file_total_lines < min_lines
        {
            continue;
        }
        if let Some(max_lines) = args.max_file_lines
            && file_total_lines > max_lines
        {
            continue;
        }

        if let Some(pattern) = &args.file_pattern
            && !file_path.to_string_lossy().contains(pattern)
        {
            continue;
        }

        // Read file content once for code/doc matching and snippet extraction
        let file_content = fs::read_to_string(&file_path).ok();
        let file_lines: Option<Vec<&str>> = file_content.as_ref().map(|c| c.lines().collect());

        // Filter symbols within the file and collect SymbolSearchResult directly with match info
        let mut filtered_symbol_results: Vec<SymbolSearchResult> = Vec::new();
        for symbol in file_symbols {
            // Apply symbol kind filter
            if let Some(kinds) = &args.symbol_kinds
                && !kinds.is_empty()
                && !kinds.contains(&symbol.kind.as_str().to_string())
            {
                continue;
            }

            // Apply function lines filter
            if let Some(min_func_lines) = args.min_function_lines {
                if let Some(func_lines) = symbol.function_lines {
                    if func_lines < min_func_lines {
                        continue;
                    }
                } else {
                    // For symbols without function_lines data, skip if we have a minimum requirement
                    continue;
                }
            }

            if let Some(max_func_lines) = args.max_function_lines {
                if let Some(func_lines) = symbol.function_lines {
                    if func_lines > max_func_lines {
                        continue;
                    }
                } else {
                    // For symbols without function_lines data, skip if we have a maximum requirement
                    continue;
                }
            }

            // Prepare match spans and scoring
            let mut match_spans: Vec<MatchSpan> = Vec::new();
            let mut score: f64 = 0.0;

            // Determine allowed fields to search (default: all)
            let allowed_fields: std::collections::HashSet<String> = args
                .fields
                .as_ref()
                .map(|v| v.iter().map(|s| s.to_lowercase()).collect())
                .unwrap_or_else(|| {
                    ["name", "keyword", "code", "doc"]
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect()
                });

            // Helper to find term in symbol's code region (returns (field, line_no, col, matched_text))
            let find_in_code = |term_lower: &str| -> Option<(String, usize, usize, String)> {
                if let Some(lines) = &file_lines {
                    let start_idx = symbol.start_line.saturating_sub(1);
                    let end_idx = symbol.end_line.min(lines.len());
                    for i in start_idx..end_idx {
                        let line = lines[i];
                        let line_lower = line.to_lowercase();
                        if let Some(col) = line_lower.find(term_lower) {
                            let matched = &line[col..col + term_lower.len()];
                            let trimmed = line.trim_start();
                            let is_doc = trimmed.starts_with("///")
                                || trimmed.starts_with("//")
                                || trimmed.starts_with("/*")
                                || trimmed.starts_with('#');
                            let field = if is_doc { "doc" } else { "code" };
                            return Some((field.to_string(), i + 1, col + 1, matched.to_string()));
                        }
                    }
                }
                None
            };

            // Apply name search filter (if provided)
            if let Some(name_searches) = &args.name {
                let mut found = false;
                for name_search in name_searches {
                    let term = name_search.to_lowercase();

                    // name field
                    if allowed_fields.contains("name") && symbol.name.to_lowercase().contains(&term)
                    {
                        found = true;
                        match_spans.push(MatchSpan {
                            field: "name".to_string(),
                            start_line: symbol.start_line,
                            end_line: symbol.start_line,
                            start_col: None,
                            end_col: None,
                            matched_text: name_search.clone(),
                        });
                        score += 0.7;
                        break;
                    }

                    // keyword field
                    if !found && allowed_fields.contains("keyword") {
                        for keyword in &symbol.keywords {
                            if keyword.to_lowercase().contains(&term) {
                                found = true;
                                match_spans.push(MatchSpan {
                                    field: "keyword".to_string(),
                                    start_line: symbol.start_line,
                                    end_line: symbol.start_line,
                                    start_col: None,
                                    end_col: None,
                                    matched_text: keyword.clone(),
                                });
                                score += 0.2;
                                break;
                            }
                        }
                    }

                    // code/doc fields
                    if !found
                        && (allowed_fields.contains("code") || allowed_fields.contains("doc"))
                        && let Some((field, line_no, col, matched_text)) = find_in_code(&term)
                        && allowed_fields.contains(&field)
                    {
                        found = true;
                        match_spans.push(MatchSpan {
                            field: field.clone(),
                            start_line: line_no,
                            end_line: line_no,
                            start_col: Some(col),
                            end_col: Some(col + matched_text.len()),
                            matched_text,
                        });
                        if field == "doc" {
                            score += 0.4;
                        } else {
                            score += 0.3;
                        }
                        break;
                    }
                }

                if !found {
                    continue;
                }
            }

            // Apply keyword search filter (if provided)
            if let Some(keyword_searches) = &args.keyword_search {
                let mut found_any = false;
                for keyword_search in keyword_searches {
                    let term = keyword_search.to_lowercase();

                    // keyword field
                    if allowed_fields.contains("keyword") {
                        for keyword in &symbol.keywords {
                            if keyword.to_lowercase().contains(&term) {
                                found_any = true;
                                match_spans.push(MatchSpan {
                                    field: "keyword".to_string(),
                                    start_line: symbol.start_line,
                                    end_line: symbol.start_line,
                                    start_col: None,
                                    end_col: None,
                                    matched_text: keyword.clone(),
                                });
                                score += 0.2;
                                break;
                            }
                        }
                        if found_any {
                            break;
                        }
                    }

                    // name field
                    if !found_any && allowed_fields.contains("name") {
                        if symbol.name.to_lowercase().contains(&term) {
                            found_any = true;
                            match_spans.push(MatchSpan {
                                field: "name".to_string(),
                                start_line: symbol.start_line,
                                end_line: symbol.start_line,
                                start_col: None,
                                end_col: None,
                                matched_text: symbol.name.clone(),
                            });
                            score += 0.7;
                        }
                        if found_any {
                            break;
                        }
                    }

                    // code/doc
                    if !found_any
                        && (allowed_fields.contains("code") || allowed_fields.contains("doc"))
                        && let Some((field, line_no, col, matched_text)) = find_in_code(&term)
                        && allowed_fields.contains(&field)
                    {
                        found_any = true;
                        match_spans.push(MatchSpan {
                            field: field.clone(),
                            start_line: line_no,
                            end_line: line_no,
                            start_col: Some(col),
                            end_col: Some(col + matched_text.len()),
                            matched_text,
                        });
                        if field == "doc" {
                            score += 0.4;
                        } else {
                            score += 0.3;
                        }
                        break;
                    }
                }

                if !found_any {
                    continue;
                }
            }

            // Build the SymbolSearchResult from the symbol and attach match info
            let mut sres = SymbolSearchResult::from(symbol);
            if !match_spans.is_empty() {
                sres.match_score = Some(score.min(1.0));
                sres.matches = match_spans;
            }

            filtered_symbol_results.push(sres);
        }

        // Apply symbol count filters
        let symbol_count = filtered_symbol_results.len();

        // Skip files with no matching symbols
        if symbol_count == 0 {
            continue;
        }

        if let Some(min_symbols) = args.min_symbols_per_file
            && symbol_count < min_symbols
        {
            continue;
        }
        if let Some(max_symbols) = args.max_symbols_per_file
            && symbol_count > max_symbols
        {
            continue;
        }

        // Add code snippets if requested
        let include_snippets = args.include_snippets.unwrap_or(true);
        if include_snippets && let Ok(content) = fs::read_to_string(&file_path) {
            let lines: Vec<&str> = content.lines().collect();
            let context_lines = args.context_lines.unwrap_or(0);
            let snippet_max_chars = args.snippet_max_chars.unwrap_or(1000);

            for symbol_result in &mut filtered_symbol_results {
                let start_line = symbol_result.start_line.saturating_sub(1);
                let end_line = symbol_result.end_line.min(lines.len());

                let start = start_line.saturating_sub(context_lines);
                let end = (end_line + context_lines).min(lines.len());

                if start < end {
                    let mut snippet = lines[start..end].join("\n");
                    if snippet.len() > snippet_max_chars {
                        snippet.truncate(snippet_max_chars);
                        snippet.push_str("...");
                    }
                    symbol_result.code_snippet = snippet;
                }
            }
        }

        // Compute file_match_score as max of symbol match_score
        let file_match_score = filtered_symbol_results
            .iter()
            .filter_map(|s| s.match_score)
            .fold(None, |acc: Option<f64>, v| match acc {
                None => Some(v),
                Some(prev) => Some(prev.max(v)),
            });

        results.push(RepomapSearchResult {
            file: file_path,
            file_total_lines,
            symbols: filtered_symbol_results,
            symbol_count,
            file_match_score,
        });
    }

    // Sort results
    let sort_desc = args.sort_desc.unwrap_or(true);
    if let Some(sort_by) = &args.sort_by {
        match sort_by.as_str() {
            "file_lines" => {
                results.sort_by(|a, b| {
                    if sort_desc {
                        b.file_total_lines.cmp(&a.file_total_lines)
                    } else {
                        a.file_total_lines.cmp(&b.file_total_lines)
                    }
                });
            }
            "symbol_count" => {
                results.sort_by(|a, b| {
                    if sort_desc {
                        b.symbol_count.cmp(&a.symbol_count)
                    } else {
                        a.symbol_count.cmp(&b.symbol_count)
                    }
                });
            }
            "file_path" => {
                results.sort_by(|a, b| {
                    if sort_desc {
                        b.file.cmp(&a.file)
                    } else {
                        a.file.cmp(&b.file)
                    }
                });
            }
            "function_lines" => {
                // Sort by the largest function in each file
                results.sort_by(|a, b| {
                    let a_max_func = a
                        .symbols
                        .iter()
                        .filter_map(|s| s.function_lines)
                        .max()
                        .unwrap_or(0);
                    let b_max_func = b
                        .symbols
                        .iter()
                        .filter_map(|s| s.function_lines)
                        .max()
                        .unwrap_or(0);
                    if sort_desc {
                        b_max_func.cmp(&a_max_func)
                    } else {
                        a_max_func.cmp(&b_max_func)
                    }
                });
            }
            _ => {} // No sorting for unknown criteria
        }
    }

    // Apply limit
    let limit = args.limit.unwrap_or(20);
    results.truncate(limit);

    results
}
