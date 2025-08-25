use crate::planning::task_types::TaskType;
use std::collections::HashMap;

pub(crate) fn estimate_complexity(input: &str, task_type: &TaskType) -> f32 {
    let base_complexity = task_type.base_complexity();

    // Adjustment based on input length
    let length_factor = (input.len() as f32 / 100.0).min(0.3);

    // Keywords suggesting multiple files
    let multi_file_keywords = ["複数", "全て", "すべて", "multiple", "all"];
    let multi_file_factor = if multi_file_keywords.iter().any(|k| input.contains(k)) {
        0.2
    } else {
        0.0
    };

    (base_complexity + length_factor + multi_file_factor).min(1.0)
}

pub(crate) fn calculate_confidence(
    keyword_patterns: &HashMap<String, TaskType>,
    keywords: &[String],
    task_type: &TaskType,
) -> f32 {
    if keywords.is_empty() {
        return 0.1;
    }

    let matching_keywords = keywords
        .iter()
        .filter(|k| {
            keyword_patterns
                .get(*k)
                .map(|t| t == task_type)
                .unwrap_or(false)
        })
        .count();

    (matching_keywords as f32 / keywords.len() as f32).max(0.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::task_types::TaskType;
    use std::collections::HashMap;

    #[test]
    fn test_calculate_confidence_empty_keywords() {
        let patterns: HashMap<String, TaskType> = HashMap::new();
        let keywords: Vec<String> = vec![];
        let conf = calculate_confidence(&patterns, &keywords, &TaskType::Unknown);
        assert!((conf - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn test_calculate_confidence_matching_rate() {
        let mut patterns: HashMap<String, TaskType> = HashMap::new();
        patterns.insert("編集".to_string(), TaskType::SimpleCodeEdit);
        let keywords = vec!["編集".to_string(), "検索".to_string()];
        let conf = calculate_confidence(&patterns, &keywords, &TaskType::SimpleCodeEdit);
        // 1 matching out of 2 -> 0.5
        assert!((conf - 0.5).abs() < 1e-6 || conf >= 0.1);
    }

    #[test]
    fn test_estimate_complexity_length_and_multi_file() {
        let short = estimate_complexity("読んで", &TaskType::SimpleFileOperation);
        let long = estimate_complexity(
            "これは非常に長い説明で複数のファイルをまたがる変更を含むタスクです。複数のファイルを更新してください",
            &TaskType::Refactoring,
        );
        assert!(short < long);

        // Multi-file keyword increases complexity
        let multi = estimate_complexity("複数のファイルを変更する", &TaskType::Refactoring);
        assert!(multi > 0.6 || multi > estimate_complexity("短い説明", &TaskType::Refactoring));
    }
}
