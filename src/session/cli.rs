//! `dgc session` CLI subcommands.
//!
//! Provides non-interactive session management: listing, inspecting, and
//! deleting persisted sessions without launching the TUI.

use crate::config::AppConfig;
use crate::session::{SessionStore, format};
use anyhow::{Context, Result, anyhow};
use clap::Subcommand;

#[derive(Subcommand, Debug, Clone)]
pub enum SessionCommands {
    /// List sessions (most recently updated first)
    List,
    /// Show details of a session
    Show { id: String },
    /// Delete a session
    Delete {
        id: String,
        /// Skip the confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
}

fn session_store(cfg: &AppConfig) -> Result<SessionStore> {
    SessionStore::new(cfg.project_root.join(".doge/sessions"))
        .with_context(|| format!("open session store under {}", cfg.project_root.display()))
}

fn resolve(store: &SessionStore, id: &str) -> Result<String> {
    store.resolve_id_prefix(id).map_err(|e| {
        if matches!(e, crate::session::error::SessionError::NotFound(_)) {
            anyhow!(
                "Session not found: {}\nHint: run `dgc session list` to see available sessions.",
                id
            )
        } else {
            anyhow::anyhow!(e)
        }
    })
}

fn confirm(message: &str) -> bool {
    if !stdin_is_tty() {
        eprintln!("Refusing to proceed without a TTY; re-run with --yes to skip confirmation.");
        return false;
    }
    eprint!("{} [y/N] ", message);
    use std::io::Write;
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_ok() {
        matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
    } else {
        false
    }
}

fn stdin_is_tty() -> bool {
    use std::io::IsTerminal;
    // `is_terminal` on stdin tells us whether an interactive user is present.
    std::io::stdin().is_terminal()
}

/// Entry point for the `dgc session` subcommands.
pub fn run(cfg: AppConfig, command: SessionCommands) -> Result<()> {
    let store = session_store(&cfg)?;
    match command {
        SessionCommands::List => {
            let summaries = store.list_with_stats()?;
            println!("{}", format::format_summary_list(&summaries, None));
        }
        SessionCommands::Show { id } => {
            let id = resolve(&store, &id)?;
            let data = store
                .load(&id)
                .with_context(|| format!("load session {}", id))?;
            println!("{}", format::format_detail(&data));
        }
        SessionCommands::Delete { id, yes } => {
            let id = resolve(&store, &id)?;
            if !yes && !confirm(&format!("Delete session {}? This cannot be undone.", id)) {
                println!("Aborted.");
                return Ok(());
            }
            store.delete(&id)?;
            println!("Deleted session {}", id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionData;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn test_session_list_prints_table() {
        let dir = tempdir().expect("Failed to create temp directory");
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        };
        let store = session_store(&cfg).expect("Failed to create store");
        let mut data = SessionData::new();
        data.meta.title = "CLI test session".to_string();
        data.add_conversation_entry(HashMap::new());
        store.save(&data).expect("Failed to save session");

        let summaries = store.list_with_stats().expect("Failed to list");
        let out = format::format_summary_list(&summaries, None);
        assert!(out.contains("CLI test session"));
        assert!(out.contains(&format::short_id(&data.meta.id)));
    }

    #[test]
    fn test_resolve_error_mentions_hint() {
        let dir = tempdir().expect("Failed to create temp directory");
        let cfg = AppConfig {
            project_root: dir.path().to_path_buf(),
            ..AppConfig::default()
        };
        let store = session_store(&cfg).expect("Failed to create store");
        let err = resolve(&store, "does-not-exist").expect_err("Should fail");
        let msg = format!("{}", err);
        assert!(msg.contains("Session not found"));
        assert!(msg.contains("dgc session list"));
    }
}
