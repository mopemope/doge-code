//! Key representation (independent of the terminal library) and the
//! single-line editor behind the input box and dialog fields.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
}

impl KeyInput {
    /// `key` without modifiers.
    pub fn new(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
        }
    }

    pub fn char(c: char) -> Self {
        Self::new(Key::Char(c))
    }

    /// Ctrl + `c`.
    pub fn ctrl(c: char) -> Self {
        Self {
            ctrl: true,
            ..Self::char(c)
        }
    }

    /// A plain printable character (no ctrl/alt).
    pub fn printable(&self) -> Option<char> {
        match self.key {
            Key::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }
}

/// A one-line text field with a cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEditor {
    text: String,
    /// Byte offset of the cursor, always on a char boundary.
    cursor: usize,
}

impl LineEditor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Number of characters before the cursor (for placing the cursor).
    pub fn cursor_chars(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    /// Replace the content; the cursor goes to the end.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_owned();
        self.cursor = self.text.len();
    }

    /// The content, leaving the editor empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    /// Insert at the cursor; control characters (a newline included) are ignored.
    pub fn insert(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
            self.text.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Delete everything before the cursor.
    pub fn kill_to_start(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Apply an editing key: characters, Backspace, Delete, Left, Right,
    /// Home, End and Ctrl-U. Returns whether the key was an editing key.
    pub fn handle(&mut self, key: &KeyInput) -> bool {
        if key.ctrl {
            if key.key == Key::Char('u') {
                self.kill_to_start();
                return true;
            }
            return false;
        }
        if let Some(c) = key.printable() {
            self.insert(c);
            return true;
        }
        match key.key {
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Left => self.left(),
            Key::Right => self.right(),
            Key::Home => self.home(),
            Key::End => self.end(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(editor: &mut LineEditor, text: &str) {
        for c in text.chars() {
            assert!(editor.handle(&KeyInput::char(c)));
        }
    }

    #[test]
    fn line_editor_edits() {
        let mut e = LineEditor::default();
        type_str(&mut e, "héllo");
        assert_eq!(e.text(), "héllo");
        assert_eq!(e.cursor_chars(), 5);

        e.handle(&KeyInput::new(Key::Left));
        e.handle(&KeyInput::new(Key::Left));
        e.handle(&KeyInput::char('X'));
        assert_eq!(e.text(), "hélXlo");
        e.handle(&KeyInput::new(Key::Backspace));
        assert_eq!(e.text(), "héllo");
        e.handle(&KeyInput::new(Key::Delete));
        assert_eq!(e.text(), "hélo");

        e.handle(&KeyInput::new(Key::Home));
        assert_eq!(e.cursor_chars(), 0);
        e.handle(&KeyInput::new(Key::Backspace));
        assert_eq!(e.text(), "hélo", "backspace at the start does nothing");
        e.handle(&KeyInput::new(Key::End));
        e.handle(&KeyInput::new(Key::Delete));
        assert_eq!(e.text(), "hélo", "delete at the end does nothing");
        e.handle(&KeyInput::new(Key::Right));
        assert_eq!(e.cursor_chars(), 4);

        e.handle(&KeyInput::new(Key::Left));
        e.handle(&KeyInput::ctrl('u'));
        assert_eq!(e.text(), "o", "ctrl-u deletes before the cursor");
        assert_eq!(e.cursor_chars(), 0);

        assert_eq!(e.take(), "o");
        assert!(e.is_empty());
    }

    #[test]
    fn line_editor_ignores_controls_and_other_keys() {
        let mut e = LineEditor::default();
        e.insert('\n');
        e.insert('\x1b');
        assert!(e.is_empty());
        assert!(!e.handle(&KeyInput::new(Key::Enter)));
        assert!(!e.handle(&KeyInput::new(Key::Up)));
        assert!(!e.handle(&KeyInput::ctrl('c')));
        assert!(e.is_empty());
    }
}
