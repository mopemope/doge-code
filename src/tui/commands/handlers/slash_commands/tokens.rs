use crate::llm::client_core::OpenAIClient;
use crate::llm::prompt_cache::format_hit_ratio_percent;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /tokens to the dedicated handler.
/// This separation improves modularity by isolating command logic.
pub fn handle_tokens(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    if let Some(client) = &executor.client {
        let last_prompt = client.get_prompt_tokens_used();
        let session_prompt = client.get_total_prompt_tokens_used();
        let session_total = client.get_total_tokens_used();
        if client.has_reasoning_usage() {
            let last_reasoning = client.get_reasoning_tokens_used();
            let session_reasoning = client.get_total_reasoning_tokens_used();
            ui.push_log(format!(
                "Session totals: {session_prompt} prompt tokens, {session_reasoning} reasoning tokens, {session_total} total"
            ));
            ui.push_log(format!(
                "Last request: {last_prompt} prompt tokens, {last_reasoning} reasoning tokens"
            ));
        } else {
            ui.push_log(format!(
                "Session totals: {session_prompt} prompt tokens ({session_total} total incl. completions)"
            ));
            ui.push_log(format!("Last request prompt size: {last_prompt} tokens"));
        }
        for line in session_cache_lines(client) {
            ui.push_log(line);
        }
        for line in last_request_cache_lines(client) {
            ui.push_log(line);
        }
        if !client.has_cached_prompt_usage() && !client.has_cache_write_usage() {
            ui.push_log("Prompt cache metrics: not reported by provider".to_string());
        }
        let window = executor.cfg.get_context_window_size();
        if let Some(window) = window {
            let remaining = window.saturating_sub(last_prompt);
            ui.push_log(format!(
                "Remaining context: ~{remaining} tokens (window: {window})"
            ));
        }
    } else {
        ui.push_log("No LLM client available.");
    }
}

/// Session-level cache lines for `/tokens`.
///
/// Returns an empty vec when the provider never reported cache telemetry;
/// the caller emits the "not reported" notice instead. Never emits a `0`
/// line for an unreported provider.
pub fn session_cache_lines(client: &OpenAIClient) -> Vec<String> {
    let has_cached = client.has_cached_prompt_usage();
    let has_write = client.has_cache_write_usage();
    if !has_cached && !has_write {
        return Vec::new();
    }
    let total_cached = client.get_total_cached_prompt_tokens();
    let total_written = client.get_total_cache_write_tokens();
    let ratio = client.prompt_cache_hit_ratio();
    let ratio_text = ratio.and_then(format_hit_ratio_percent);
    let line = if has_cached && has_write {
        match ratio_text {
            Some(r) => format!(
                "Prompt cache: {total_cached} cached, {total_written} written ({r} read ratio)"
            ),
            None => format!("Prompt cache: {total_cached} cached, {total_written} written"),
        }
    } else if has_cached {
        match ratio_text {
            Some(r) => {
                format!("Prompt cache: {total_cached} cached prompt tokens ({r} hit ratio)")
            }
            None => format!("Prompt cache: {total_cached} cached prompt tokens"),
        }
    } else {
        format!("Prompt cache: {total_written} written prompt tokens")
    };
    vec![line]
}

/// Last-request cache lines for `/tokens`.
///
/// Returns an empty vec when the last response carried no cache details.
/// When the session has telemetry but the last request did not, the caller
/// should surface that explicitly (see below) rather than reusing a stale
/// value.
pub fn last_request_cache_lines(client: &OpenAIClient) -> Vec<String> {
    let last = client.last_prompt_cache_usage();
    let last_ratio = client.last_prompt_cache_hit_ratio();
    let ratio_text = last_ratio.and_then(format_hit_ratio_percent);
    match (last.cached_tokens, last.cache_write_tokens) {
        (Some(cached), Some(written)) => vec![match ratio_text {
            Some(r) => {
                format!("Last request cache: {cached} cached, {written} written ({r} hit ratio)")
            }
            None => format!("Last request cache: {cached} cached, {written} written"),
        }],
        (Some(cached), None) => vec![match ratio_text {
            Some(r) => format!("Last request cache: {cached} cached ({r} hit ratio)"),
            None => format!("Last request cache: {cached} cached"),
        }],
        (None, Some(written)) => {
            vec![format!("Last request cache: {written} written")]
        }
        (None, None) => {
            if client.has_cached_prompt_usage() || client.has_cache_write_usage() {
                vec!["Last request cache: not reported for this request".to_string()]
            } else {
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{PromptTokensDetails, Usage};

    fn usage(prompt: u32, cached: Option<u32>, written: Option<u32>) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: 10,
            total_tokens: prompt + 10,
            prompt_tokens_details: if cached.is_some() || written.is_some() {
                Some(PromptTokensDetails {
                    cached_tokens: cached,
                    cache_write_tokens: written,
                    extra: Default::default(),
                })
            } else {
                None
            },
            completion_tokens_details: None,
        }
    }

    fn client_with(usages: &[Usage]) -> OpenAIClient {
        let client = OpenAIClient::new("https://api.example.com/", "x").unwrap();
        for u in usages {
            client.record_usage(u);
        }
        client
    }

    #[test]
    fn test_session_cache_line_with_hit_ratio() {
        let client = client_with(&[usage(10_000, Some(8000), None)]);
        let lines = session_cache_lines(&client);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("8000 cached"), "line: {}", lines[0]);
        assert!(lines[0].contains("80.0%"), "line: {}", lines[0]);
        let last = last_request_cache_lines(&client);
        assert_eq!(last.len(), 1);
        assert!(last[0].contains("8000 cached"), "last: {}", last[0]);
    }

    #[test]
    fn test_session_cache_line_with_write() {
        let client = client_with(&[usage(10_000, Some(8000), Some(2000))]);
        let lines = session_cache_lines(&client);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("8000 cached"), "line: {}", lines[0]);
        assert!(lines[0].contains("2000 written"), "line: {}", lines[0]);
        let last = last_request_cache_lines(&client);
        assert!(last[0].contains("8000 cached"));
        assert!(last[0].contains("2000 written"));
    }

    #[test]
    fn test_no_cache_metrics_yields_no_lines() {
        let client = client_with(&[usage(100, None, None)]);
        assert!(session_cache_lines(&client).is_empty());
        assert!(last_request_cache_lines(&client).is_empty());
        assert!(!client.has_cached_prompt_usage());
        assert!(!client.has_cache_write_usage());
    }

    #[test]
    fn test_stale_last_request_not_reused() {
        let client = client_with(&[usage(10_000, Some(8000), None), usage(12_000, None, None)]);
        // Session line still shows accumulated telemetry.
        let session = session_cache_lines(&client);
        assert_eq!(session.len(), 1);
        assert!(session[0].contains("8000 cached"));
        // Last-request line must not repeat the stale 8000.
        let last = last_request_cache_lines(&client);
        assert_eq!(last.len(), 1);
        assert!(last[0].contains("not reported"), "last: {}", last[0]);
        assert!(!last[0].contains("8000"));
    }

    #[test]
    fn test_explicit_zero_shows_zero_not_missing() {
        let client = client_with(&[usage(10_000, Some(0), None)]);
        let session = session_cache_lines(&client);
        assert!(session[0].contains("0 cached"));
        let last = last_request_cache_lines(&client);
        assert!(last[0].contains("0 cached"));
    }
}
