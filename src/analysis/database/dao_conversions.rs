use crate::analysis::database::entities::file_hash::ActiveModel as FileHashActiveModel;
use crate::analysis::database::entities::symbol_info::ActiveModel as SymbolInfoActiveModel;
use crate::analysis::database::entities::symbol_info::Model as SymbolInfoModel;
use crate::analysis::symbol::SymbolInfo as AnalysisSymbolInfo;
use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::Set;
use std::path::PathBuf;

/// Converts an AnalysisSymbolInfo to a SymbolInfo ActiveModel for database insertion.
pub fn symbol_to_active_model(
    symbol: &AnalysisSymbolInfo,
    project_root: &str,
) -> Result<SymbolInfoActiveModel> {
    let file_path_str = symbol
        .file
        .to_str()
        .context("File path is not valid UTF-8")?;

    // Convert keywords to a JSON string
    let keywords_json =
        serde_json::to_string(&symbol.keywords).unwrap_or_else(|_| "[]".to_string());

    Ok(SymbolInfoActiveModel {
        id: Default::default(), // Auto-increment
        name: Set(symbol.name.clone()),
        kind: Set(match symbol.kind {
            crate::analysis::symbol::SymbolKind::Function => "fn".to_string(),
            crate::analysis::symbol::SymbolKind::Struct => "struct".to_string(),
            crate::analysis::symbol::SymbolKind::Enum => "enum".to_string(),
            crate::analysis::symbol::SymbolKind::Trait => "trait".to_string(),
            crate::analysis::symbol::SymbolKind::Impl => "impl".to_string(),
            crate::analysis::symbol::SymbolKind::Method => "method".to_string(),
            crate::analysis::symbol::SymbolKind::AssocFn => "assoc_fn".to_string(),
            crate::analysis::symbol::SymbolKind::Mod => "mod".to_string(),
            crate::analysis::symbol::SymbolKind::Variable => "var".to_string(),
            crate::analysis::symbol::SymbolKind::Comment => "comment".to_string(),
        }),
        file_path: Set(file_path_str.to_string()),
        start_line: Set(symbol.start_line as i32),
        start_col: Set(symbol.start_col as i32),
        end_line: Set(symbol.end_line as i32),
        end_col: Set(symbol.end_col as i32),
        parent: Set(symbol.parent.clone()),
        file_total_lines: Set(symbol.file_total_lines as i32),
        function_lines: Set(symbol.function_lines.map(|l| l as i32)),
        project_root: Set(project_root.to_string()),
        keywords: Set(keywords_json),
        created_at: Set(Utc::now()),
    })
}

/// Converts a SymbolInfo Model from the database to an AnalysisSymbolInfo.
pub fn active_model_to_symbol(model: SymbolInfoModel) -> Result<AnalysisSymbolInfo> {
    let kind = match model.kind.as_str() {
        "fn" => crate::analysis::symbol::SymbolKind::Function,
        "struct" => crate::analysis::symbol::SymbolKind::Struct,
        "enum" => crate::analysis::symbol::SymbolKind::Enum,
        "trait" => crate::analysis::symbol::SymbolKind::Trait,
        "impl" => crate::analysis::symbol::SymbolKind::Impl,
        "method" => crate::analysis::symbol::SymbolKind::Method,
        "assoc_fn" => crate::analysis::symbol::SymbolKind::AssocFn,
        "mod" => crate::analysis::symbol::SymbolKind::Mod,
        "var" => crate::analysis::symbol::SymbolKind::Variable,
        "comment" => crate::analysis::symbol::SymbolKind::Comment,
        _ => {
            return Err(anyhow::anyhow!(
                "unexpected value for SymbolKind: {}",
                model.kind
            ));
        }
    };

    // Parse keywords from JSON string
    let keywords: Vec<String> =
        serde_json::from_str(&model.keywords).unwrap_or_else(|_| Vec::new());

    Ok(AnalysisSymbolInfo {
        name: model.name,
        kind,
        file: PathBuf::from(model.file_path),
        start_line: model.start_line as usize,
        start_col: model.start_col as usize,
        end_line: model.end_line as usize,
        end_col: model.end_col as usize,
        parent: model.parent,
        file_total_lines: model.file_total_lines as usize,
        function_lines: model.function_lines.map(|l| l as usize),
        keywords,
    })
}

/// Converts file path and hash to a FileHash ActiveModel for database insertion.
pub fn file_hash_to_active_model(
    file_path: &str,
    hash: &str,
    project_root: &str,
) -> FileHashActiveModel {
    FileHashActiveModel {
        id: Default::default(), // Auto-increment
        file_path: Set(file_path.to_string()),
        hash: Set(hash.to_string()),
        project_root: Set(project_root.to_string()),
        created_at: Set(Utc::now()),
    }
}
