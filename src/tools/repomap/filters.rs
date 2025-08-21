use crate::analysis::SymbolInfo;
use crate::tools::repomap::types::SearchRepomapArgs;

pub fn group_by_file(symbols: Vec<SymbolInfo>) -> std::collections::HashMap<std::path::PathBuf, Vec<SymbolInfo>> {
    let mut file_groups = std::collections::HashMap::new();
    for symbol in symbols {
        file_groups
            .entry(symbol.file.clone())
            .or_default()
            .push(symbol);
    }
    file_groups
}

pub fn filter_symbols_for_file(mut file_symbols: Vec<SymbolInfo>, args: &SearchRepomapArgs) -> Vec<SymbolInfo> {
    let mut filtered = Vec::new();
    for symbol in file_symbols.drain(..) {
        if let Some(kinds) = &args.symbol_kinds {
            if !kinds.contains(&symbol.kind.as_str().to_string()) {
                continue;
            }
        }

        if let Some(min_func_lines) = args.min_function_lines {
            if let Some(func_lines) = symbol.function_lines {
                if func_lines < min_func_lines {
                    continue;
                }
            } else {
                continue;
            }
        }

        if let Some(max_func_lines) = args.max_function_lines {
            if let Some(func_lines) = symbol.function_lines {
                if func_lines > max_func_lines {
                    continue;
                }
            } else {
                continue;
            }
        }

        filtered.push(symbol);
    }
    filtered
}
