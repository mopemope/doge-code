use crate::llm::client_core::OpenAIClient;
use crate::llm::prompt_cache::format_hit_ratio_percent;
use crate::llm::usage_ledger::UsageLedger;
use crate::tui::commands::core::TuiExecutor;
use crate::tui::view::TuiApp;

/// Delegate /tokens to the dedicated handler.
/// Persisted session usage is the source of truth for session totals;
/// current-process client totals and last-request telemetry are separate.
pub fn handle_tokens(executor: &mut TuiExecutor, ui: &mut TuiApp) {
    // Persisted session usage: survives resume and session switches.
    let persisted: Option<crate::session::SessionData> =
        crate::utils::safe_std_lock(&executor.session_manager, "session_manager")
            .ok()
            .and_then(|mgr| mgr.current_session.clone());
    match &persisted {
        Some(session) => match &session.usage {
            Some(ledger) => {
                ui.push_log(format!(
                    "Persisted session usage: {} input, {} output, {} total, {} attempts, {} provider usage reports",
                    ledger.prompt_tokens,
                    ledger.completion_tokens,
                    ledger.total_tokens,
                    ledger.attempts,
                    ledger.usage_records
                ));
                let unknown = ledger.unknown_usage_attempts();
                if unknown > 0 {
                    ui.push_log(format!(
                        "Usage coverage: {unknown} request(s) has no provider usage report"
                    ));
                }
                if ledger.historical_usage_unknown {
                    ui.push_log(
                        "Persisted session usage is incomplete: legacy historical usage is unknown."
                            .to_string(),
                    );
                }
                for line in persisted_cache_lines(ledger) {
                    ui.push_log(line);
                }
                for line in persisted_reasoning_lines(ledger) {
                    ui.push_log(line);
                }
                if ledger.cached_usage_records == 0 && ledger.cache_write_usage_records == 0 {
                    ui.push_log("Persisted session cache: not reported by provider".to_string());
                }
                if ledger.reasoning_usage_records == 0 {
                    ui.push_log("Persisted reasoning: not reported by provider".to_string());
                }
            }
            None => {
                ui.push_log(
                    "Persisted session usage is incomplete: legacy historical usage is unknown."
                        .to_string(),
                );
            }
        },
        None => {
            ui.push_log("No active session.".to_string());
        }
    }

    if let Some(client) = &executor.client {
        // Current-process client totals: may span sessions after a switch.
        let snapshot = client.usage_snapshot();
        ui.push_log(format!(
            "Current process client totals: {} input, {} output, {} total, {} attempts, {} provider usage reports (may span sessions)",
            snapshot.prompt_tokens,
            snapshot.completion_tokens,
            snapshot.total_tokens,
            snapshot.attempts,
            snapshot.usage_records
        ));
        let unknown = snapshot.unknown_usage_attempts();
        if unknown > 0 {
            ui.push_log(format!(
                "Current process coverage: {unknown} request(s) has no provider usage report"
            ));
        }
        // Last-request telemetry stays client-side, separate from persistence.
        let last_prompt = client.get_prompt_tokens_used();
        if client.has_reasoning_usage() {
            let last_reasoning = client.get_reasoning_tokens_used();
            ui.push_log(format!(
                "Last request: {last_prompt} prompt tokens, {last_reasoning} reasoning tokens"
            ));
            // Persisted reasoning line already shows session totals above;
            // also surface the process total for completeness.
            ui.push_log(format!(
                "Current process reasoning total: {} tokens",
                client.get_total_reasoning_tokens_used()
            ));
        } else {
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
        ui.push_log("No LLM client available.".to_string());
    }
}

/// Persisted-session cache telemetry from the session ledger.
///
/// Uses `cached_tokens` / `cache_write_tokens` with their report counts, so
/// an explicit `0` stays `0` while a missing report stays unknown. Coverage
/// is shown as `reported for X/Y responses`.
pub fn persisted_cache_lines(ledger: &UsageLedger) -> Vec<String> {
    if ledger.cached_usage_records == 0 && ledger.cache_write_usage_records == 0 {
        return Vec::new();
    }
    let total_prompt = ledger.prompt_tokens;
    let ratio = if ledger.cached_usage_records > 0 && total_prompt > 0 {
        ledger.cached_tokens.and_then(|cached| {
            let r = cached as f64 / total_prompt as f64;
            if r.is_finite() {
                format_hit_ratio_percent(r)
            } else {
                None
            }
        })
    } else {
        None
    };
    let cached_part = ledger.cached_tokens.map(|v| {
        format!(
            "{v} cached (reported for {}/{} responses)",
            ledger.cached_usage_records, ledger.usage_records
        )
    });
    let written_part = ledger.cache_write_tokens.map(|v| {
        format!(
            "{v} written (reported for {}/{} responses)",
            ledger.cache_write_usage_records, ledger.usage_records
        )
    });
    let body = match (cached_part, written_part) {
        (Some(c), Some(w)) => format!("{c}, {w}"),
        (Some(c), None) => c,
        (None, Some(w)) => w,
        (None, None) => format!(
            "reported for {}/{} responses",
            ledger.cached_usage_records + ledger.cache_write_usage_records,
            ledger.usage_records
        ),
    };
    let line = match ratio {
        Some(r) => format!("Persisted session cache: {body} ({r} read ratio)"),
        None => format!("Persisted session cache: {body}"),
    };
    vec![line]
}

/// Persisted-session reasoning telemetry from the session ledger.
///
/// `None` (no report) never renders as `0 tokens`; an explicit `Some(0)`
/// does.
pub fn persisted_reasoning_lines(ledger: &UsageLedger) -> Vec<String> {
    if ledger.reasoning_usage_records == 0 {
        return Vec::new();
    }
    match ledger.reasoning_tokens {
        Some(tokens) => vec![format!(
            "Persisted reasoning: {tokens} tokens (reported for {}/{} responses)",
            ledger.reasoning_usage_records, ledger.usage_records
        )],
        None => vec![format!(
            "Persisted reasoning: reported for {}/{} responses",
            ledger.reasoning_usage_records, ledger.usage_records
        )],
    }
}

/// Session-level cache lines for `/tokens` (current process).
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
                "Current process cache: {total_cached} cached, {total_written} written ({r} read ratio)"
            ),
            None => {
                format!("Current process cache: {total_cached} cached, {total_written} written")
            }
        }
    } else if has_cached {
        match ratio_text {
            Some(r) => {
                format!(
                    "Current process cache: {total_cached} cached prompt tokens ({r} hit ratio)"
                )
            }
            None => format!("Current process cache: {total_cached} cached prompt tokens"),
        }
    } else {
        format!("Current process cache: {total_written} written prompt tokens")
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

    #[allow(clippy::too_many_arguments)]
    fn persisted_ledger(
        total: u64,
        attempts: u64,
        records: u64,
        cached: Option<u64>,
        cached_records: u64,
        reasoning: Option<u64>,
        reasoning_records: u64,
        historical: bool,
    ) -> UsageLedger {
        UsageLedger {
            attempts,
            usage_records: records,
            prompt_tokens: total.saturating_sub(20),
            completion_tokens: 20.min(total),
            total_tokens: total,
            reasoning_tokens: reasoning,
            cached_tokens: cached,
            cache_write_tokens: None,
            reasoning_usage_records: reasoning_records,
            cached_usage_records: cached_records,
            cache_write_usage_records: 0,
            historical_usage_unknown: historical,
        }
    }

    #[test]
    fn persisted_zero_vs_unknown_cache() {
        // Explicit zero stays zero with coverage.
        let explicit = persisted_ledger(1000, 1, 1, Some(0), 1, None, 0, false);
        let lines = persisted_cache_lines(&explicit);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("0 cached"), "line: {}", lines[0]);
        assert!(lines[0].contains("1/1"), "line: {}", lines[0]);
        // Missing report yields no cache line (caller shows not-reported).
        let missing = persisted_ledger(1000, 1, 1, None, 0, None, 0, false);
        assert!(persisted_cache_lines(&missing).is_empty());
    }

    #[test]
    fn persisted_reasoning_unknown_is_not_zero() {
        let missing = persisted_ledger(1000, 1, 1, None, 0, None, 0, false);
        assert!(persisted_reasoning_lines(&missing).is_empty());
        let explicit_zero = persisted_ledger(1000, 1, 1, None, 0, Some(0), 1, false);
        let lines = persisted_reasoning_lines(&explicit_zero);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("0 tokens"), "line: {}", lines[0]);
    }

    #[test]
    fn tokens_resume_shows_persisted_with_fresh_client() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let tools = crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(cfg.clone()));
        let manager = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session::SessionManager::with_store(
                crate::session::SessionStore::new(dir.path().join("sessions")).unwrap(),
            ),
        ));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
            let id = mgr.current_session_id().unwrap();
            let delta = persisted_ledger(1000, 2, 2, Some(200), 1, Some(50), 1, false);
            mgr.apply_usage_delta(&id, &delta).unwrap();
        }
        let mut executor = crate::tui::commands::core::TuiExecutor::construct_with_session_manager(
            cfg, repomap, tools, manager,
        )
        .unwrap();
        // Fresh process client with no usage.
        executor.client = Some(OpenAIClient::new("https://api.example.com/", "x").unwrap());
        let mut ui = crate::tui::view::TuiApp::new_for_test("tokens", None, "default");
        super::handle_tokens(&mut executor, &mut ui);
        let logs: String = ui
            .log
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(logs.contains("Persisted session usage"), "logs: {logs}");
        assert!(
            logs.contains("1000"),
            "persisted total must survive resume, logs: {logs}"
        );
        assert!(
            logs.contains("Current process client totals"),
            "process totals separated, logs: {logs}"
        );
        assert!(
            !logs.contains("Session totals:"),
            "must not mislabel process as session"
        );
    }

    #[test]
    fn tokens_unknown_attempt_and_historical_flag() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::AppConfig {
            project_root: dir.path().to_path_buf(),
            ..Default::default()
        };
        let repomap = std::sync::Arc::new(tokio::sync::RwLock::new(None));
        let tools = crate::tools::FsTools::new(repomap.clone(), std::sync::Arc::new(cfg.clone()));
        let manager = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session::SessionManager::with_store(
                crate::session::SessionStore::new(dir.path().join("sessions")).unwrap(),
            ),
        ));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.create_session(None).unwrap();
            let id = mgr.current_session_id().unwrap();
            // 2 attempts, 1 record => 1 unknown; historical unknown set.
            let mut delta = persisted_ledger(500, 2, 1, None, 0, None, 0, false);
            delta.historical_usage_unknown = true;
            mgr.apply_usage_delta(&id, &delta).unwrap();
        }
        let mut executor = crate::tui::commands::core::TuiExecutor::construct_with_session_manager(
            cfg, repomap, tools, manager,
        )
        .unwrap();
        executor.client = Some(OpenAIClient::new("https://api.example.com/", "x").unwrap());
        let mut ui = crate::tui::view::TuiApp::new_for_test("tokens", None, "default");
        super::handle_tokens(&mut executor, &mut ui);
        let logs: String = ui
            .log
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<String>>()
            .join("\n");
        assert!(
            logs.contains("has no provider usage report"),
            "unknown attempt shown, logs: {logs}"
        );
        assert!(
            logs.contains("legacy historical usage is unknown"),
            "historical flag shown, logs: {logs}"
        );
        assert!(
            !logs.contains("0 tokens") || logs.contains("not reported"),
            "no false zero"
        );
    }
}
