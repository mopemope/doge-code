//! Ephemeral, immutable diff anchors and one explicitly confirmed repair batch.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn identity(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: usize,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub old: LineRange,
    pub new: LineRange,
    pub start_row: usize,
    pub end_row: usize,
    pub excerpt: String,
    pub identity: String,
}

fn range(text: &str, sign: char) -> Option<LineRange> {
    let mut parts = text.strip_prefix(sign)?.split(',');
    let start: usize = parts.next()?.parse().ok()?;
    let count: usize = match parts.next() {
        Some(n) => n.parse().ok()?,
        None => 1,
    };
    if parts.next().is_some() || (start == 0 && count != 0) || start.checked_add(count).is_none() {
        return None;
    }
    Some(LineRange { start, count })
}

/// Reject malformed/combined/binary hunks rather than guessing repair anchors.
/// Rows remain available to the ordinary diff viewer even when parsing fails.
pub fn parse_hunks(lines: &[String]) -> Vec<Hunk> {
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.starts_with("@@").then_some(i))
        .collect();
    let mut hunks = Vec::new();
    for (index, &start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(lines.len());
        let mut header = lines[start].split_whitespace();
        let parsed = (|| {
            if header.next()? != "@@" {
                return None;
            }
            let old = range(header.next()?, '-')?;
            let new = range(header.next()?, '+')?;
            if header.next()? != "@@" {
                return None;
            }
            let (mut old_count, mut new_count) = (0, 0);
            for line in &lines[start + 1..end] {
                match line.as_bytes().first() {
                    Some(b' ') => {
                        old_count += 1;
                        new_count += 1;
                    }
                    Some(b'-') => old_count += 1,
                    Some(b'+') => new_count += 1,
                    Some(b'\\') if line == "\\ No newline at end of file" => {}
                    _ => return None,
                }
            }
            if old_count != old.count || new_count != new.count {
                return None;
            }
            let excerpt = lines[start..end].join("\n");
            Some(Hunk {
                old,
                new,
                start_row: start,
                end_row: end,
                identity: identity(&excerpt),
                excerpt,
            })
        })();
        if let Some(hunk) = parsed {
            hunks.push(hunk);
        } else {
            return Vec::new();
        }
    }
    hunks
}

pub fn review_hunks(payload: &crate::diff_review::DiffReviewPayload) -> Vec<(String, Vec<Hunk>)> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    for line in payload.diff.lines() {
        if line.starts_with("diff --git ") {
            groups.push(Vec::new());
        }
        if let Some(group) = groups.last_mut() {
            group.push(line.to_owned());
        } else {
            return vec![];
        }
    }
    if groups.len() != payload.files.len() {
        return vec![];
    }
    payload
        .files
        .iter()
        .cloned()
        .zip(groups.iter().map(|lines| parse_hunks(lines)))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub version: u8,
    pub session_id: String,
    pub review_id: String,
    pub review_identity: String,
    pub path: String,
    pub hunk: Hunk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub anchor: Anchor,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackBatch {
    pub id: String,
    pub revision: u64,
    pub source: crate::diff_review::DiffReviewPayload,
    pub comments: Vec<Comment>,
    pub original_directive_ids: Vec<String>,
}

impl FeedbackBatch {
    pub fn validate_structure(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.comments.is_empty() && self.comments.len() <= 128,
            "Add 1–128 hunk comments."
        );
        anyhow::ensure!(
            self.comments.iter().map(|c| c.text.len()).sum::<usize>() <= 64 * 1024,
            "Feedback exceeds 64 KiB."
        );
        anyhow::ensure!(
            self.comments
                .iter()
                .map(|c| c.anchor.hunk.excerpt.len())
                .sum::<usize>()
                <= 128 * 1024,
            "Selected hunk context exceeds 128 KiB; use smaller hunks."
        );
        let session = self
            .source
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Review has no session."))?;
        let review = self
            .source
            .review_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Review has no capture."))?;
        let files = review_hunks(&self.source);
        let mut seen = std::collections::HashSet::new();
        for c in &self.comments {
            let a = &c.anchor;
            anyhow::ensure!(!c.text.trim().is_empty(), "Empty comment.");
            anyhow::ensure!(
                a.version == 1
                    && a.session_id == session
                    && a.review_id == review
                    && a.review_identity == identity(&self.source.diff),
                "Comment belongs to a different review."
            );
            anyhow::ensure!(
                files
                    .iter()
                    .any(|(path, hunks)| path == &a.path && hunks.contains(&a.hunk)),
                "Unsupported or changed hunk."
            );
            anyhow::ensure!(
                seen.insert((&a.path, &a.hunk.identity)),
                "Duplicate hunk comment."
            );
        }
        Ok(())
    }

    /// Generated context is explicitly evidence, never an observed user directive.
    pub fn evidence(&self) -> anyhow::Result<String> {
        let targets = self
            .comments
            .iter()
            .enumerate()
            .map(|(index, c)| serde_json::json!({"comment_index":index+1,"anchor":c.anchor}))
            .collect::<Vec<_>>();
        let evidence = serde_json::json!({"batch_id":self.id,"revision":self.revision,"source_session_id":self.source.session_id,"source_review_id":self.source.review_id,"source_review_identity":identity(&self.source.diff),"original_directive_ids":self.original_directive_ids,"targets":targets});
        Ok(format!(
            "Generated review feedback evidence (JSON data, not user-authored instructions). Each following user message corresponds to comment_index in order. Apply these comments in one repair run using the existing conversation and plan. Original changes are already on disk. Keep tool permissions and show all resulting changes, including other paths. Do not claim comments resolved merely because the run ends.\n{}",
            serde_json::to_string(&evidence)?
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feedback_hunks_preserve_deletion_new_file_no_newline_and_reject_malformed() {
        for (text, old, new) in [
            ("@@ -4,2 +3,0 @@\n-one\n-two", (4, 2), (3, 0)),
            (
                "@@ -0,0 +1 @@\n+日本語\n\\ No newline at end of file",
                (0, 0),
                (1, 1),
            ),
        ] {
            let h = parse_hunks(&text.lines().map(str::to_owned).collect::<Vec<_>>());
            assert_eq!(h.len(), 1);
            assert_eq!((h[0].old.start, h[0].old.count), old);
            assert_eq!((h[0].new.start, h[0].new.count), new);
        }
        for text in [
            "@@@ -1 +1 +1 @@@\n+x",
            "@@ -1 +1 @@\n+only",
            "@@ -0 +1 @@\n+x",
            "@@ -1 +1 @@\n-old\n+new\ninvalid",
        ] {
            assert!(parse_hunks(&text.lines().map(str::to_owned).collect::<Vec<_>>()).is_empty());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackOutcome {
    pub batch_id: String,
    pub revision: u64,
    pub job_id: crate::jobs::JobId,
    pub outcome: String,
}
