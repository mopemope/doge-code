use anyhow::Result;
use crossterm::{
    cursor, queue,
    style::{ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

use crate::tui::state::{TuiApp, build_render_plan}; // TuiAppをインポート

// TuiAppに描画のロジックを実装
impl TuiApp {
    pub fn draw_with_model(&self, model: Option<&str>) -> Result<()> {
        let mut stdout = io::stdout();
        let (w, h) = terminal::size()?;
        let plan = build_render_plan(
            &self.title,
            self.status,
            &self.log,
            &self.input,
            w,
            h,
            model,
        );

        // Draw header (2 lines)
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(first) = plan.header_lines.first() {
            queue!(stdout, SetForegroundColor(self.theme.header_fg))?;
            write!(stdout, "{first}")?;
            queue!(stdout, ResetColor)?;
        }
        queue!(
            stdout,
            cursor::MoveTo(0, 1),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        if let Some(second) = plan.header_lines.get(1) {
            queue!(stdout, SetForegroundColor(self.theme.header_separator))?;
            write!(stdout, "{second}")?;
            queue!(stdout, ResetColor)?;
        }

        // Draw log area starting at row 2 up to h-2
        let start_row = 2u16;
        let max_rows = h.saturating_sub(2).saturating_sub(1); // leave one line for input
        let mut in_code_block = false; // 新規: コードブロック内かどうかのフラグ
        for (i, line) in plan.log_lines.iter().take(max_rows as usize).enumerate() {
            let row = start_row + i as u16;
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            let cmp = line.as_str();

            // 新規: コードブロック識別子のチェック
            if cmp.starts_with(" [CodeBlockStart(") && cmp.ends_with(")]") {
                // コードブロック開始識別子: 特別な描画はしないが、フラグを立てる
                in_code_block = true;
                // この行自体は描画しないか、非常に薄い色で描画するなどして非表示に近づける
                // ここでは描画をスキップする
                continue;
            } else if cmp == " [CodeBlockEnd]" {
                // コードブロック終了識別子: フラグを下ろす
                in_code_block = false;
                // この行自体は描画しない
                continue;
            }

            // 新規: コードブロック内の行に異なるスタイルを適用
            if in_code_block {
                // コードブロック行: テーマで定義された背景色を使用
                queue!(stdout, SetBackgroundColor(self.theme.llm_code_block_bg))?;
                queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?; // 文字色は維持
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            }
            // 既存の描画ロジック: 順番を変更して、コードブロック判定を最優先に
            else if cmp.starts_with("> ") {
                queue!(stdout, SetForegroundColor(self.theme.user_input_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.contains("[Cancelled]")
                || cmp.contains("[cancelled]")
                || cmp.contains("[canceled]")
            {
                queue!(stdout, SetForegroundColor(self.theme.error_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.starts_with('[') {
                queue!(stdout, SetForegroundColor(self.theme.info_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.starts_with("LLM error:")
                || cmp.contains("error")
                || cmp.contains("Error")
            {
                queue!(stdout, SetForegroundColor(self.theme.error_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else if cmp.contains("warning") || cmp.contains("Warning") {
                queue!(stdout, SetForegroundColor(self.theme.warning_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            } else {
                // LLM response lines: darker grey/black background with white foreground for contrast
                queue!(stdout, SetBackgroundColor(self.theme.llm_response_bg))?;
                queue!(stdout, SetForegroundColor(self.theme.llm_response_fg))?;
                write!(stdout, "{line}")?;
                queue!(stdout, ResetColor)?;
            }
        }
        // Clear any remaining rows in the log area if current content is shorter
        let used_rows = plan.log_lines.len() as u16;
        for row in start_row + used_rows..start_row + max_rows {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
        }

        // Draw input line at bottom
        let input_row = h.saturating_sub(1);
        queue!(
            stdout,
            cursor::MoveTo(0, input_row),
            terminal::Clear(ClearType::CurrentLine)
        )?;
        write!(stdout, "{}", plan.input_line)?;

        // draw completion popup above input if visible
        if self.compl.visible && !self.compl.items.is_empty() {
            let popup_h = std::cmp::min(self.compl.items.len(), 10) as u16;
            for i in 0..popup_h as usize {
                let row = input_row.saturating_sub(1 + i as u16);
                let item = &self.compl.items[i];
                let mark = if i == self.compl.selected { ">" } else { " " };
                let line = format!(
                    "{mark} {}  [{}]",
                    item.rel,
                    item.ext.clone().unwrap_or_default()
                );
                queue!(
                    stdout,
                    cursor::MoveTo(0, row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                if i == self.compl.selected {
                    queue!(
                        stdout,
                        SetBackgroundColor(self.theme.completion_selected_bg),
                        SetForegroundColor(self.theme.completion_selected_fg)
                    )?;
                } else {
                    queue!(stdout, SetForegroundColor(self.theme.completion_item_fg))?;
                }
                write!(stdout, "{line}")?;
                if i == self.compl.selected {
                    queue!(stdout, ResetColor)?;
                }
            }
        }

        // Position terminal cursor at visual end of input line using unicode width
        let col = UnicodeWidthStr::width(plan.input_line.as_str()) as u16;
        queue!(stdout, cursor::MoveTo(col, input_row), cursor::Show)?;

        stdout.flush()?;
        Ok(())
    }
}
