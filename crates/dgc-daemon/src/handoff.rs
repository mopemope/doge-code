//! Assemble a handoff from the recorded conversation and local Git state.

use std::io;
use std::path::Path;
use std::process::Stdio;

use dgc_protocol::MAX_EVENT_TEXT_BYTES;
use dgc_protocol::event::{AgentEvent, EventKind};
use dgc_protocol::model::{Agent, SendBehavior};
use tokio::io::{self as async_io, AsyncReadExt};
use tokio::time::timeout;

use crate::provider::resolve::executable_name;
use crate::workspace::{GIT_TIMEOUT, local_command};

pub(crate) const SCAN_LIMIT: u32 = 1000;
const PROMPT_BYTES: usize = 32 * 1024;
const SECTION_BYTES: usize = 4 * 1024;
const FIRST_INSTRUCTION_BYTES: usize = 16 * 1024;
const FIRST_TRUNCATED: &str = "…(truncated; the full instruction is in the log below)";
const OMITTED: &str = "…(earlier text omitted)\n";

/// Chunks from one send are consecutive with the same behavior. A size
/// split can be distinguished from ordinary adjacent steers by testing
/// whether the next character could have fitted in the previous event.
fn continues_chunk(previous: &str, next: &str) -> bool {
    let Some(c) = next.chars().next() else {
        return false;
    };
    if previous.len() < MAX_EVENT_TEXT_BYTES / 6 {
        return false;
    }
    let previous_bytes = serde_json::to_string(previous).map_or(0, |s| s.len() - 2);
    let next_bytes = serde_json::to_string(&c).map_or(0, |s| s.len() - 2);
    previous_bytes + next_bytes > MAX_EVENT_TEXT_BYTES
}

struct Instruction {
    first_seq: u64,
    last_seq: u64,
    text: String,
}

fn instructions(events: &[AgentEvent]) -> Vec<Instruction> {
    let mut messages: Vec<Instruction> = Vec::new();
    let mut previous: Option<(u64, SendBehavior, &str)> = None;
    for event in events {
        if let EventKind::UserMessage { text, behavior } = &event.kind {
            if let Some(message) = messages.last_mut()
                && previous.is_some_and(|(seq, b, chunk)| {
                    seq.checked_add(1) == Some(event.seq)
                        && b == *behavior
                        && continues_chunk(chunk, text)
                })
            {
                message.last_seq = event.seq;
                message.text.push_str(text);
            } else {
                messages.push(Instruction {
                    first_seq: event.seq,
                    last_seq: event.seq,
                    text: text.clone(),
                });
            }
            previous = Some((event.seq, *behavior, text));
        }
    }
    messages.dedup_by(|a, b| a.text == b.text);
    messages
}

fn last_answer(events: &[AgentEvent]) -> String {
    let Some(turn) = events
        .rsplit(|event| matches!(event.kind, EventKind::TurnStarted))
        .find(|turn| {
            !turn.iter().any(|event| matches!(event.kind, EventKind::RateLimited { .. } | EventKind::Error { .. }))
                && turn.iter().any(|event| matches!(&event.kind, EventKind::AssistantText { text, .. } if !text.is_empty()))
        })
    else {
        return String::new();
    };
    let mut answer = String::new();
    let mut finished_block = false;
    for event in turn {
        if let EventKind::AssistantText { text, final_ } = &event.kind {
            if finished_block {
                answer.push('\n');
            }
            answer.push_str(text);
            finished_block = *final_;
        }
    }
    answer
}

pub(crate) fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            matches!(c, '\n' | '\t')
                || (!c.is_control()
                    && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        })
        .collect()
}

fn head(text: &str, max: usize) -> &str {
    &text[..text.floor_char_boundary(max)]
}

fn text_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    if max <= OMITTED.len() {
        return String::new();
    }
    let start = text.ceil_char_boundary(text.len() - (max - OMITTED.len()));
    format!("{OMITTED}{}", &text[start..])
}

pub(crate) fn build_prompt(
    source: &Agent,
    first_page: &[AgentEvent],
    tail: &[AgentEvent],
    status: &str,
    diff: &str,
    log_path: &Path,
) -> Option<String> {
    let initial = first_page
        .first()
        .filter(|event| event.seq == 1)
        .and_then(|_| instructions(first_page).into_iter().next());
    let initial_text = initial.as_ref().map_or_else(
        || "(unavailable in the retained event log)".to_owned(),
        |message| strip_controls(&message.text),
    );
    let provider = executable_name(source.provider);
    let title = strip_controls(&source.title);
    let fixed = format!(
        "You are continuing work started by another coding agent ({provider}) in this repository. \
         Review the state below, then continue the task.\n\nTitle and first instruction\n{title}\n"
    );
    let log = format!(
        "\n\nPrevious event log\nThe full log of the previous agent is at {}.\n",
        strip_controls(&log_path.to_string_lossy()),
    );
    let recent_heading = "\n\nRecent user instructions\n";
    let answer_heading = "\n\nLast assistant answer\n";
    let status_heading = "\n\nWorkspace changes\ngit status --short\n";
    let diff_heading = "\ngit diff --stat\n";
    let fixed_len = fixed.len()
        + log.len()
        + recent_heading.len()
        + answer_heading.len()
        + status_heading.len()
        + diff_heading.len();
    let budget = PROMPT_BYTES.checked_sub(fixed_len)?;
    let (initial_text, initial_marker) =
        if initial_text.len() <= FIRST_INSTRUCTION_BYTES.min(budget) {
            (initial_text.as_str(), "")
        } else if budget >= FIRST_TRUNCATED.len() {
            (
                head(
                    &initial_text,
                    FIRST_INSTRUCTION_BYTES.min(budget - FIRST_TRUNCATED.len()),
                ),
                FIRST_TRUNCATED,
            )
        } else {
            ("", "")
        };
    let budget = budget - initial_text.len() - initial_marker.len();

    let recent = instructions(tail);
    let recent = recent[recent.len().saturating_sub(3)..]
        .iter()
        .filter(|message| {
            initial.as_ref().is_none_or(|first| {
                !(first.first_seq..=first.last_seq).contains(&message.first_seq)
            })
        })
        .map(|message| strip_controls(&message.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let answer = strip_controls(&last_answer(tail));
    let status = strip_controls(status);
    let diff = strip_controls(diff);
    let status = head(&status, SECTION_BYTES);
    let diff = head(&diff, SECTION_BYTES);
    let git_bytes = status.len() + diff.len();

    // Reserve Git space first, then recent instructions, then the answer.
    // When the recent section must shrink, keep its newest text.
    let recent = text_tail(&recent, budget.saturating_sub(git_bytes));
    let answer = text_tail(
        &answer,
        SECTION_BYTES.min(budget.saturating_sub(git_bytes + recent.len())),
    );
    let status = head(status, budget);
    let diff = head(diff, budget - status.len());
    Some(format!(
        "{fixed}{initial_text}{initial_marker}{recent_heading}{recent}{answer_heading}{answer}{status_heading}{status}{diff_heading}{diff}{log}"
    ))
}

pub(crate) async fn git_state(cwd: &Path) -> (String, String) {
    tokio::join!(
        git_summary(cwd, "status", &["--short"]),
        git_summary(
            cwd,
            "diff",
            &["--stat", "--no-color", "--no-ext-diff", "--no-textconv"]
        ),
    )
}

async fn git_summary(cwd: &Path, command: &str, args: &[&str]) -> String {
    match timeout(GIT_TIMEOUT, capture_git(cwd, command, args)).await {
        Ok(Ok(summary)) => summary,
        Ok(Err(error)) => format!("(git {command} unavailable: {error})"),
        Err(_) => format!("(git {command} unavailable: timed out after 30 s)"),
    }
}

async fn capture_git(cwd: &Path, command: &str, args: &[&str]) -> Result<String, String> {
    let kind = |error: io::Error| error.kind().to_string();
    let mut child = local_command("git", cwd)
        .args([
            "--no-pager",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "color.ui=false",
            // status has no --no-color option and its own setting overrides color.ui.
            "-c",
            "color.status=false",
        ])
        .arg(command)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(kind)?;
    let mut stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let mut bytes = Vec::new();
    // Drain the rest so a large result neither allocates more nor blocks Git.
    (&mut stdout)
        .take(SECTION_BYTES as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(kind)?;
    async_io::copy(&mut stdout, &mut async_io::sink())
        .await
        .map_err(kind)?;
    let status = child.wait().await.map_err(kind)?;
    if status.success() {
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        Err(status.to_string())
    }
}
