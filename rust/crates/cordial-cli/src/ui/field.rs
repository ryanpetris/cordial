//! A single-line text editor shared by the shell prompt and the TUI's
//! pairing-code field. Long values scroll horizontally.
use crate::ui::text::width;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, Default)]
pub struct Field {
    chars: Vec<char>,
    cursor: usize,
    /// First character shown when the value is wider than the view.
    offset: usize,
    /// Maximum characters; zero for no limit.
    pub limit: usize,
}

impl Field {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
    /// Replaces the value, filtered as [`Field::insert`] filters typed text.
    pub fn set_value(&mut self, value: &str) {
        self.chars.clear();
        self.cursor = 0;
        self.insert(value);
    }
    pub fn reset(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.offset = 0;
    }

    /// Inserts text as typed or pasted: controls are dropped, and line breaks
    /// and tabs become spaces.
    pub fn insert(&mut self, text: &str) {
        for c in text.chars() {
            let c = match c {
                '\n' | '\r' | '\t' => ' ',
                c if c.is_control() => continue,
                c => c,
            };
            if self.limit > 0 && self.chars.len() >= self.limit {
                break;
            }
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    fn word_start(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }
    fn word_end(&self) -> usize {
        let mut i = self.cursor;
        while i < self.chars.len() && self.chars[i].is_whitespace() {
            i += 1;
        }
        while i < self.chars.len() && !self.chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Applies an editing key; false for keys the field does not use.
    pub fn key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.chars.len(),
            KeyCode::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(),
            KeyCode::Char('k') if ctrl => self.chars.truncate(self.cursor),
            KeyCode::Char('u') if ctrl => {
                self.chars.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let start = self.word_start();
                self.chars.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('d') if alt => {
                let end = self.word_end();
                self.chars.drain(self.cursor..end);
            }
            KeyCode::Char('h') if ctrl => return self.key(&KeyEvent::from(KeyCode::Backspace)),
            KeyCode::Char(c) if !ctrl && !alt => self.insert(&c.to_string()),
            KeyCode::Backspace if alt || ctrl => {
                let start = self.word_start();
                self.chars.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Backspace => {}
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete => {}
            KeyCode::Left if alt || ctrl => self.cursor = self.word_start(),
            KeyCode::Right if alt || ctrl => self.cursor = self.word_end(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            _ => return false,
        }
        true
    }

    /// The part of the value that fits `cells`, keeping the cursor visible,
    /// and the cursor's column within it.
    pub fn view(&mut self, cells: usize) -> (String, usize) {
        let cells = cells.max(1);
        let w = |cs: &[char]| width(&cs.iter().collect::<String>());
        self.offset = self.offset.min(self.cursor);
        // The cursor needs one cell after the text before it.
        while self.offset < self.cursor && w(&self.chars[self.offset..self.cursor]) + 1 > cells {
            self.offset += 1;
        }
        let mut end = self.offset;
        while end < self.chars.len() && w(&self.chars[self.offset..=end]) <= cells {
            end += 1;
        }
        let column = w(&self.chars[self.offset..self.cursor]);
        (self.chars[self.offset..end].iter().collect(), column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(f: &mut Field, code: KeyCode, modifiers: KeyModifiers) {
        assert!(f.key(&KeyEvent::new(code, modifiers)));
    }

    #[test]
    fn edits_and_limits() {
        let mut f = Field::new(6);
        f.insert("12\n34567");
        assert_eq!(f.value(), "12 345");
        press(&mut f, KeyCode::Backspace, KeyModifiers::NONE);
        press(&mut f, KeyCode::Home, KeyModifiers::NONE);
        press(&mut f, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(f.value(), "2 34");
        let mut f = Field::new(0);
        f.set_value("pair \"Test keyboard\"");
        press(&mut f, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(f.value(), "pair \"Test ");
        press(&mut f, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(f.is_empty());
        f.insert("a\u{1b}[2Jb");
        assert_eq!(f.value(), "a[2Jb");
        f.set_value("x\u{9b}2J\ty\u{1b}");
        assert_eq!(f.value(), "x2J y");
    }

    #[test]
    fn view_scrolls_to_the_cursor() {
        let mut f = Field::new(0);
        f.set_value("0123456789");
        assert_eq!(f.view(5), ("6789".into(), 4));
        press(&mut f, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(f.view(5), ("01234".into(), 0));
        f.set_value("键键键");
        assert_eq!(f.view(4), ("键".into(), 2));
    }
}
