use crate::planning::task_types::TaskType;
use std::collections::HashMap;

pub(crate) fn initialize_patterns(keyword_patterns: &mut HashMap<String, TaskType>) {
    // File operations
    let file_ops = vec![
        "読む",
        "read",
        "読み込み",
        "表示",
        "show",
        "見る",
        "確認",
        "書く",
        "write",
        "作成",
        "create",
        "保存",
        "save",
    ];
    for keyword in file_ops {
        keyword_patterns.insert(keyword.to_string(), TaskType::SimpleFileOperation);
    }

    // Search operations
    let search_ops = vec![
        "検索",
        "search",
        "探す",
        "find",
        "調べる",
        "investigate",
        "grep",
    ];
    for keyword in search_ops {
        keyword_patterns.insert(keyword.to_string(), TaskType::SimpleSearch);
    }

    // Edit operations
    let edit_ops = vec![
        "編集", "edit", "修正", "fix", "変更", "change", "更新", "update", "追加", "add", "削除",
        "delete", "remove",
    ];
    for keyword in edit_ops {
        keyword_patterns.insert(keyword.to_string(), TaskType::SimpleCodeEdit);
    }

    // Refactoring operations
    let refactor_ops = vec![
        "リファクタ",
        "refactor",
        "整理",
        "cleanup",
        "分割",
        "split",
        "統合",
        "merge",
        "最適化",
        "optimize",
    ];
    for keyword in refactor_ops {
        keyword_patterns.insert(keyword.to_string(), TaskType::Refactoring);
    }

    // Implementation operations
    let impl_ops = vec![
        "実装",
        "implement",
        "機能",
        "feature",
        "新しい",
        "new",
        "開発",
        "develop",
        "構築",
        "build",
    ];
    for keyword in impl_ops {
        keyword_patterns.insert(keyword.to_string(), TaskType::FeatureImplementation);
    }
}

pub(crate) fn extract_keywords(
    keyword_patterns: &HashMap<String, TaskType>,
    input: &str,
) -> Vec<String> {
    let input_lower = input.to_lowercase();

    let mut keywords = Vec::new();

    // Check for partial matches across the entire input text for each pattern
    for pattern in keyword_patterns.keys() {
        if input_lower.contains(pattern) {
            keywords.push(pattern.clone());
        }
    }

    // Add stem matching for Japanese conjugations
    let japanese_stems = [
        ("読ん", "読む"),
        ("書い", "書く"),
        ("作っ", "作成"),
        ("見", "見る"),
        ("編集", "編集"),
        ("修正", "修正"),
        ("変更", "変更"),
        ("追加", "追加"),
        ("削除", "削除"),
        ("検索", "検索"),
        ("探", "探す"),
        ("調べ", "調べる"),
    ];

    for (stem, base) in &japanese_stems {
        if input_lower.contains(stem)
            && keyword_patterns.contains_key(*base)
            && !keywords.contains(&base.to_string())
        {
            keywords.push(base.to_string());
        }
    }

    keywords.sort();
    keywords.dedup();
    keywords
}

pub(crate) fn classify_by_keywords(
    keyword_patterns: &HashMap<String, TaskType>,
    keywords: &[String],
) -> TaskType {
    use std::collections::HashMap as HMap;

    let mut type_scores: HMap<TaskType, usize> = HMap::new();

    for keyword in keywords {
        if let Some(task_type) = keyword_patterns.get(keyword) {
            *type_scores.entry(task_type.clone()).or_insert(0) += 1;
        }
    }

    // Select the task type with the highest score
    type_scores
        .into_iter()
        .max_by_key(|(_, score)| *score)
        .map(|(task_type, _)| task_type)
        .unwrap_or(TaskType::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_extract_keywords_stem_and_case() {
        let mut patterns = HashMap::new();
        initialize_patterns(&mut patterns);

        // Japanese stems
        let input = "ファイルを読んで編集してください";
        let keywords = extract_keywords(&patterns, input);
        assert!(keywords.contains(&"読む".to_string()));
        assert!(keywords.contains(&"編集".to_string()));

        // English case-insensitive
        let input = "Please Read the file and then Edit it";
        let keywords = extract_keywords(&patterns, input);
        assert!(keywords.contains(&"read".to_string()));
        assert!(keywords.contains(&"edit".to_string()));

        // Dedup and sort
        let input = "読 読 読ん 読ん";
        let keywords = extract_keywords(&patterns, input);
        // Should contain '読む' only once
        let count = keywords.iter().filter(|k| *k == "読む").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_classify_by_keywords_priority() {
        let mut patterns = HashMap::new();
        initialize_patterns(&mut patterns);

        // If both '編集' (SimpleCodeEdit) and '検索' (SimpleSearch) are present,
        // classification should pick the task type with higher count or first occurrence logic.
        let keywords = vec!["編集".to_string(), "検索".to_string(), "編集".to_string()];
        let task_type = classify_by_keywords(&patterns, &keywords);
        assert_eq!(task_type, TaskType::SimpleCodeEdit);
    }
}
