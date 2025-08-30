pub fn infer_step_type(description: &str) -> String {
    let desc_lower = description.to_lowercase();

    if desc_lower.contains("分析") || desc_lower.contains("調査") || desc_lower.contains("確認")
    {
        "analysis".to_string()
    } else if desc_lower.contains("計画")
        || desc_lower.contains("設計")
        || desc_lower.contains("検討")
    {
        "planning".to_string()
    } else if desc_lower.contains("実装")
        || desc_lower.contains("作成")
        || desc_lower.contains("変更")
        || desc_lower.contains("追加")
    {
        "implementation".to_string()
    } else if desc_lower.contains("テスト")
        || desc_lower.contains("検証")
        || desc_lower.contains("確認")
    {
        "validation".to_string()
    } else if desc_lower.contains("クリーンアップ") || desc_lower.contains("整理") {
        "cleanup".to_string()
    } else {
        "implementation".to_string()
    }
}

pub fn infer_required_tools(description: &str) -> Vec<String> {
    let desc_lower = description.to_lowercase();
    let mut tools = Vec::new();

    if desc_lower.contains("読") || desc_lower.contains("確認") {
        tools.push("fs_read".to_string());
    }
    if desc_lower.contains("書") || desc_lower.contains("作成") {
        tools.push("fs_write".to_string());
    }
    if desc_lower.contains("編集") || desc_lower.contains("変更") {
        tools.push("edit".to_string());
    }
    if desc_lower.contains("検索") || desc_lower.contains("探") {
        tools.push("search_text".to_string());
    }
    if desc_lower.contains("実行") || desc_lower.contains("コマンド") {
        tools.push("execute_bash".to_string());
    }
    if desc_lower.contains("シンボル") || desc_lower.contains("関数") {
        tools.push("get_symbol_info".to_string());
    }

    if tools.is_empty() {
        tools.push("fs_read".to_string());
    }

    tools
}
