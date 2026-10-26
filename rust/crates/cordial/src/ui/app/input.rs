//! Text values accepted by application forms, without cursor or keyboard state.

/// A single-line form value and its character limit. Zero means no limit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Input {
    text: String,
    pub limit: usize,
}

impl Input {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    pub fn value(&self) -> String {
        self.text.clone()
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn reset(&mut self) {
        self.text.clear();
    }

    /// Accepts one line, replacing line breaks and tabs with spaces and dropping controls.
    pub fn set_value(&mut self, value: &str) {
        self.text = value
            .chars()
            .filter_map(|c| match c {
                '\n' | '\r' | '\t' => Some(' '),
                c if c.is_control() => None,
                c => Some(c),
            })
            .take(if self.limit == 0 {
                usize::MAX
            } else {
                self.limit
            })
            .collect();
    }
}
