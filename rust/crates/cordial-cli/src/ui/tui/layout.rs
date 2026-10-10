//! Styled lines with click targets: the TUI builds each frame as lines of
//! spans, remembering where every control was drawn.
use super::Action;
use crate::ui::text::width;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

// Colors come from the terminal's 16-color palette so they follow its theme.
// Every colored state also carries a text label.
pub fn plain() -> Style {
    Style::new()
}
pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}
pub fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}
pub fn accent() -> Style {
    Style::new().fg(Color::Cyan)
}
pub fn title() -> Style {
    accent().add_modifier(Modifier::BOLD)
}
pub fn ok() -> Style {
    Style::new().fg(Color::Green)
}
pub fn warn() -> Style {
    Style::new().fg(Color::Yellow)
}
pub fn err() -> Style {
    Style::new().fg(Color::Red)
}
/// Information that helps a decision, such as why a device isn't used.
pub fn info_style() -> Style {
    Style::new().fg(Color::Blue)
}
/// The selected row keeps this background across its columns.
pub fn selected() -> Style {
    Style::new().bg(Color::DarkGray)
}
/// A style that takes unset properties, such as the selection background,
/// from `base`.
pub fn inherit(style: Style, base: Style) -> Style {
    base.patch(style)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Normal,
    Primary,
    Danger,
    /// The current value among options.
    Chosen,
}
impl Tone {
    pub fn style(self) -> Style {
        match self {
            Tone::Primary => ok().add_modifier(Modifier::BOLD),
            Tone::Danger => err().add_modifier(Modifier::BOLD),
            Tone::Chosen => title(),
            Tone::Normal => bold(),
        }
    }
}

pub type Styled = Line<'static>;

pub fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}
pub fn styled(text: impl Into<String>, style: Style) -> Styled {
    Line::from(span(text, style))
}
pub fn line_width(line: &Styled) -> usize {
    line.spans.iter().map(|s| width(&s.content)).sum()
}
pub fn strip(line: &Styled) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Cuts a string to at most `cells`, by grapheme.
fn cut(s: &str, cells: usize) -> (String, bool) {
    use unicode_segmentation::UnicodeSegmentation;
    let (mut out, mut used) = (String::new(), 0);
    for g in s.graphemes(true) {
        let w = width(g);
        if used + w > cells {
            return (out, true);
        }
        used += w;
        out.push_str(g);
    }
    (out, false)
}

/// Shortens a line to `w` cells, ending in "…" when cut.
pub fn truncate(line: Styled, w: usize) -> Styled {
    if line_width(&line) <= w {
        return line;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut room = w.saturating_sub(1);
    let mut last = Style::new();
    for s in line.spans {
        last = s.style;
        let (text, cutoff) = cut(&s.content, room);
        room -= width(&text);
        if !text.is_empty() {
            out.push(span(text, s.style));
        }
        if cutoff {
            break;
        }
    }
    if w > 0 {
        out.push(span("…", last));
    }
    Line::from(out)
}
pub fn truncate_str(s: &str, w: usize) -> String {
    strip(&truncate(Line::from(s.to_owned()), w))
}

/// Pads a line with spaces to `w` cells.
pub fn pad(mut line: Styled, w: usize) -> Styled {
    let used = line_width(&line);
    if used < w {
        line.spans.push(Span::raw(" ".repeat(w - used)));
    }
    line
}
pub fn pad_str(s: &str, w: usize) -> String {
    let used = width(s);
    format!("{s}{}", " ".repeat(w.saturating_sub(used)))
}

pub fn join(mut a: Styled, b: Styled) -> Styled {
    a.spans.extend(b.spans);
    a
}

/// Places `right` at the end of a `w`-cell row, shortening `left` as needed.
pub fn spread(left: Styled, right: Styled, w: usize) -> Styled {
    let rw = line_width(&right);
    if w < rw + 1 {
        return truncate(right, w);
    }
    let room = w - rw;
    join(pad(truncate(left, room - 1), room), right)
}

/// Word-wraps text to `w` cells, breaking longer words.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let mut word = word.to_owned();
            let used = width(&line);
            let sep = usize::from(!line.is_empty());
            if used + sep + width(&word) <= w {
                if sep == 1 {
                    line.push(' ');
                }
                line.push_str(&word);
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            while width(&word) > w {
                let (head, _) = cut(&word, w);
                let head = if head.is_empty() {
                    word.chars().next().unwrap().to_string()
                } else {
                    head
                };
                word = word[head.len()..].to_owned();
                lines.push(head);
            }
            line = word;
        }
        lines.push(line);
    }
    lines
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub action: Action,
}

/// Styled lines and the click targets drawn on them; hit coordinates are
/// relative to the layout's top-left cell.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub lines: Vec<Styled>,
    pub hits: Vec<Hit>,
    pub width: usize,
}

pub struct Choice {
    pub label: String,
    pub action: Action,
    pub chosen: bool,
}

/// A switch: its state in words, which a click turns over. An unknown state turns on.
pub fn switch(on: Option<bool>, action: Action, enabled: bool) -> Layout {
    let mut l = Layout::new(8);
    let text = match on {
        Some(true) => "[✓] On",
        Some(false) => "[ ] Off",
        None => "[?] Unknown",
    };
    let look = match (enabled, on) {
        (false, _) => dim(),
        (true, Some(true)) => ok().add_modifier(Modifier::BOLD),
        (true, _) => bold(),
    };
    l.width = width(text);
    l.lines.push(Line::from(span(text, look)));
    if enabled {
        l.hits.push(Hit {
            x: 0,
            y: 0,
            w: width(text),
            action,
        });
    }
    l
}

impl Layout {
    pub fn new(width: usize) -> Self {
        Self {
            width,
            ..Self::default()
        }
    }
    pub fn line(&mut self, line: impl Into<Styled>) {
        let line = truncate(line.into(), self.width);
        self.lines.push(line);
    }
    pub fn row(&mut self) {
        self.lines.push(Line::default());
    }
    /// Word-wraps text after `prefix`, indenting continuation lines under it.
    pub fn hang(&mut self, prefix: Styled, text: &str, style: Style) {
        let indent = line_width(&prefix);
        let mut prefix = Some(prefix);
        for part in wrap(text, self.width.saturating_sub(indent).max(1)) {
            let lead = prefix
                .take()
                .unwrap_or_else(|| Line::from(" ".repeat(indent)));
            self.line(join(lead, styled(part, style)));
        }
    }
    pub fn para(&mut self, text: &str, style: Style) {
        self.hang(Line::default(), text, style);
    }
    /// The current row, made when there is none.
    pub fn last_row(&mut self) -> usize {
        self.last()
    }
    fn last(&mut self) -> usize {
        if self.lines.is_empty() {
            self.row();
        }
        self.lines.len() - 1
    }
    /// Adds a control to the current row, wrapping when it does not fit.
    pub fn button(&mut self, label: &str, action: Action, tone: Tone) {
        let text = truncate_str(&format!("[{label}]"), self.width);
        let w = width(&text);
        let mut y = self.last();
        let mut x = line_width(&self.lines[y]);
        if x > 0 {
            x += 1;
        }
        if x > 0 && x + w > self.width {
            self.row();
            y += 1;
            x = 0;
        }
        if x > 0 {
            self.lines[y].spans.push(Span::raw(" "));
        }
        self.lines[y].spans.push(span(text, tone.style()));
        self.hits.push(Hit { x, y, w, action });
    }
    /// Adds a one-line group of controls to the current row, wrapping like a button.
    pub fn append(&mut self, group: Layout) {
        let Some(line) = group.lines.into_iter().next() else {
            return;
        };
        let w = line_width(&line);
        let mut y = self.last();
        let mut x = line_width(&self.lines[y]);
        if x > 0 {
            x += 1;
        }
        if x > 0 && x + w > self.width {
            self.row();
            y += 1;
            x = 0;
        }
        if x > 0 {
            self.lines[y].spans.push(Span::raw(" "));
        }
        self.lines[y].spans.extend(line.spans);
        for h in group.hits.into_iter().filter(|h| h.y == 0) {
            self.hits.push(Hit { x: h.x + x, y, ..h });
        }
    }
    /// Adds plain text to the current row, wrapping like a button.
    pub fn label(&mut self, text: &str, style: Style) {
        let mut y = self.last();
        let (x, w) = (line_width(&self.lines[y]), width(text));
        if x > 0 && x + 1 + w > self.width {
            self.row();
            y += 1;
        } else if x > 0 {
            self.lines[y].spans.push(Span::raw(" "));
        }
        let text = truncate_str(text, self.width);
        self.lines[y].spans.push(span(text, style));
    }
    /// An unavailable control, which has no click target.
    pub fn disabled(&mut self, label: &str) {
        self.label(&format!("[{label}]"), dim());
    }
    /// Places a one-line group at the end of the current row, or of a new
    /// row when it does not fit.
    pub fn align_right(&mut self, r: Layout) {
        // Each line of the group goes at the end of a row: the first on the current row when it
        // fits there, the rest on rows of their own.
        let Layout { lines, hits, .. } = r;
        for (i, group) in lines.into_iter().enumerate() {
            let mut y = self.last();
            let (mut used, w) = (line_width(&self.lines[y]), line_width(&group));
            if i > 0 || used > 0 && used + 1 + w > self.width {
                self.row();
                y += 1;
                used = 0;
            }
            let x = self.width.saturating_sub(w);
            self.lines[y]
                .spans
                .push(Span::raw(" ".repeat(x.saturating_sub(used))));
            self.lines[y].spans.extend(group.spans);
            for h in hits.iter().filter(|h| h.y == i) {
                self.hits.push(Hit {
                    x: h.x + x,
                    y,
                    ..h.clone()
                });
            }
        }
    }
    pub fn button_right(&mut self, label: &str, action: Action, tone: Tone) {
        let mut r = Layout::new(self.width);
        r.button(label, action, tone);
        self.align_right(r);
    }
    /// Makes a whole row clickable.
    pub fn control(&mut self, line: Styled, action: Action) {
        self.hits.push(Hit {
            x: 0,
            y: self.lines.len(),
            w: self.width,
            action,
        });
        self.line(line);
    }
    /// Appends `b` below the existing lines.
    pub fn add(&mut self, b: Layout) {
        let y = self.lines.len();
        self.lines.extend(b.lines);
        self.hits
            .extend(b.hits.into_iter().map(|h| Hit { y: h.y + y, ..h }));
    }
    /// A choice drawn in place but dim and without click targets while it
    /// is unavailable, so nothing moves when it becomes available again.
    pub fn choice_if(&mut self, key: &str, kw: usize, options: Vec<Choice>, enabled: bool) {
        self.line(styled(pad_str(key, kw), dim()));
        let (mut y, mut x) = (self.lines.len() - 1, kw);
        for (i, o) in options.into_iter().enumerate() {
            let (text, mut look) = if o.chosen {
                (format!("[● {}]", o.label), Tone::Chosen.style())
            } else {
                (format!("[○ {}]", o.label), Tone::Normal.style())
            };
            if !enabled {
                look = dim();
            }
            let text = truncate_str(&text, self.width.saturating_sub(kw));
            let w = width(&text);
            if i > 0 && x + 1 + w > self.width {
                self.lines.push(Line::from(" ".repeat(kw)));
                y += 1;
                x = kw;
            } else if i > 0 {
                self.lines[y].spans.push(Span::raw(" "));
                x += 1;
            }
            self.lines[y].spans.push(span(text, look));
            if enabled {
                self.hits.push(Hit {
                    x,
                    y,
                    w,
                    action: o.action,
                });
            }
            x += w;
        }
    }
    /// A row with `label` on the left and a one-line group of controls at its end, or on the
    /// next row when both don't fit.
    pub fn labelled(&mut self, label: Styled, controls: Layout) {
        self.line(label);
        self.align_right(controls);
    }
    /// A section heading after a blank line, except at the top.
    pub fn section(&mut self, title: &str) {
        if !self.lines.is_empty() {
            self.row();
        }
        self.line(styled(title.to_owned(), bold()));
    }
    /// A fact: a dim label column, then the value, wrapped.
    pub fn fact(&mut self, label: &str, value: &str, style: Style) {
        let kw = (self.width / 3).clamp(10, 24);
        self.hang(
            styled(pad_str(&truncate_str(label, kw - 1), kw), dim()),
            value,
            style,
        );
    }
    /// A whole-row control drawn on two lines, such as a list row with a subtitle.
    pub fn control2(&mut self, first: Styled, second: Styled, action: Action) {
        let y = self.lines.len();
        for dy in 0..2 {
            self.hits.push(Hit {
                x: 0,
                y: y + dy,
                w: self.width,
                action: action.clone(),
            });
        }
        self.line(first);
        self.line(second);
    }
    /// Indents every line and hit by one cell.
    pub fn indent(&mut self) {
        for l in &mut self.lines {
            l.spans.insert(0, Span::raw(" "));
        }
        for h in &mut self.hits {
            h.x += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_and_truncation() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("", 3), [""]);
        assert_eq!(truncate_str("abcdef", 4), "abc…");
        assert_eq!(truncate_str("键键键", 4), "键…");
        let l = spread(styled("left side", plain()), styled("R", plain()), 6);
        assert_eq!(strip(&l), "lef… R");
    }

    #[test]
    fn buttons_wrap_and_align() {
        let mut l = Layout::new(12);
        l.button("One", Action::Quit, Tone::Normal);
        l.button("Two", Action::Help, Tone::Normal);
        l.button("Three", Action::AddDevice, Tone::Normal);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(
            l.hits[1],
            Hit {
                x: 6,
                y: 0,
                w: 5,
                action: Action::Help
            }
        );
        assert_eq!(l.hits[2].y, 1);
        l.button_right("X", Action::Accept, Tone::Normal);
        assert_eq!(strip(&l.lines[1]), "[Three]  [X]");
        assert_eq!(
            l.hits[3],
            Hit {
                x: 9,
                y: 1,
                w: 3,
                action: Action::Accept
            }
        );
    }
}
