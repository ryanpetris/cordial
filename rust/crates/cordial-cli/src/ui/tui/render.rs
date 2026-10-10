//! Drawing: the sidebar, the shown page with its tabs and bottom bar, the status line, and any
//! dialog or menu in front. Each frame records where every control was drawn.
use super::{
    Action, Area, Dialog, MIN_HEIGHT, MIN_WIDTH, Model, Page, Tab,
    fleet::Fleet,
    layout::{
        self, Layout, Styled, Tone, accent, bold, dim, err, join, line_width, ok, pad, span,
        spread, styled, title, truncate, warn,
    },
    spinner, words,
    world::Conn,
};
use crate::ui::text;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
};

/// A page as drawn in the content area.
pub(super) struct PageView {
    pub title: Styled,
    /// The tabs offered, in order; none on the Overview.
    pub tabs: Vec<Tab>,
    /// The tab shown.
    pub tab: Tab,
    /// Notices above every tab, with their style.
    pub banners: Vec<(String, Style)>,
    pub body: Layout,
    /// The bottom bar; empty for none.
    pub bar: Layout,
}

impl PageView {
    pub fn new(title: Styled, width: usize) -> Self {
        Self {
            title,
            tabs: Vec::new(),
            tab: Tab::Details,
            banners: Vec::new(),
            body: Layout::new(width),
            bar: Layout::new(width),
        }
    }
}

/// A dialog as drawn: its title, body and buttons.
pub(super) struct DialogView {
    pub title: String,
    pub body: Layout,
    pub buttons: Layout,
}

/// A status pill: a dot and the words, in a tone.
pub(super) fn pill(text: &str, look: Style) -> Styled {
    Line::from(vec![span("● ", look), span(text.to_owned(), look)])
}

/// The width of the sidebar for a terminal this wide.
fn sidebar_width(width: usize) -> usize {
    (width * 3 / 10).clamp(24, 34)
}

/// Writes lines into a region, clipped, returning the hits that are visible, moved to screen
/// coordinates.
fn blit(
    buf: &mut Buffer,
    rect: (usize, usize, usize, usize),
    layout: &Layout,
    scroll: usize,
) -> Vec<layout::Hit> {
    let (x, y, w, h) = rect;
    let area = buf.area;
    for (i, line) in layout.lines.iter().skip(scroll).take(h).enumerate() {
        let (lx, ly) = (x as u16, (y + i) as u16);
        if ly < area.height && lx < area.width {
            let line = truncate(line.clone(), w);
            buf.set_line(lx, ly, &line, w as u16);
        }
    }
    layout
        .hits
        .iter()
        .filter(|hit| hit.y >= scroll && hit.y < scroll + h && hit.x < w)
        .map(|hit| layout::Hit {
            x: hit.x + x,
            y: hit.y - scroll + y,
            w: hit.w.min(w - hit.x),
            action: hit.action.clone(),
        })
        .collect()
}

impl<F: Fleet> Model<F> {
    /// Paints the frame into `buf`, which covers the whole terminal.
    pub(crate) fn render(&mut self, buf: &mut Buffer) {
        self.refresh();
        buf.reset();
        let (w, h) = (self.width, self.height);
        if w < MIN_WIDTH || h < MIN_HEIGHT {
            let mut l = Layout::new(w);
            l.button("Quit", Action::Quit, Tone::Normal);
            l.para(
                &format!("Resize to at least {MIN_WIDTH} columns and {MIN_HEIGHT} rows."),
                layout::plain(),
            );
            self.hits = blit(buf, (0, 0, w, h), &l, 0);
            self.regions.clear();
            self.after_draw(buf);
            return;
        }
        let body_h = h - 1;
        let sw = sidebar_width(w);
        let mut hits = Vec::new();

        // Sidebar.
        let (side, pinned, selected) = self.sidebar(sw - 1);
        let room = body_h.saturating_sub(pinned.lines.len());
        let over = side.lines.len().saturating_sub(room);
        if self.reveal {
            if let Some(y) = selected {
                if y < self.side_scroll {
                    self.side_scroll = y;
                } else if y + 2 > self.side_scroll + room {
                    self.side_scroll = (y + 2).saturating_sub(room);
                }
            }
            self.reveal = false;
        }
        self.side_scroll = self.side_scroll.min(over);
        hits.extend(blit(buf, (0, 0, sw - 1, room), &side, self.side_scroll));
        hits.extend(blit(
            buf,
            (0, body_h - pinned.lines.len(), sw - 1, pinned.lines.len()),
            &pinned,
            0,
        ));
        self.regions = vec![(Area::Sidebar, (0, 0, sw - 1, body_h))];
        for y in 0..body_h {
            buf.set_line((sw - 1) as u16, y as u16, &styled("│", dim()), 1);
        }

        // The shown page.
        let mx = sw + 1;
        let mw = w.saturating_sub(mx + 1);
        let page = self.page_view(mw);
        let mut head = Layout::new(mw);
        head.line(page.title.clone());
        if !page.tabs.is_empty() {
            let mut tabs = Layout::new(mw);
            for t in &page.tabs {
                let look = if *t == page.tab {
                    title().add_modifier(Modifier::UNDERLINED)
                } else {
                    bold()
                };
                let label = format!(" {} ", t.label());
                let mut y = tabs.last_row();
                let mut x = line_width(&tabs.lines[y]);
                if x > 0 && x + 1 + text::width(&label) > mw {
                    tabs.row();
                    y += 1;
                    x = 0;
                }
                if x > 0 {
                    tabs.lines[y].spans.push(span(" ", Style::new()));
                    x += 1;
                }
                tabs.hits.push(layout::Hit {
                    x,
                    y,
                    w: text::width(&label),
                    action: Action::Tab(*t),
                });
                tabs.lines[y].spans.push(span(label, look));
            }
            head.add(tabs);
        }
        head.line(styled("─".repeat(mw), dim()));
        for (banner, look) in &page.banners {
            head.hang(styled("! ", *look), banner, *look);
        }
        let bar_h = if page.bar.lines.is_empty() {
            0
        } else {
            page.bar.lines.len() + 1
        };
        let top = head.lines.len();
        hits.extend(blit(buf, (mx, 0, mw, top), &head, 0));
        let room = body_h.saturating_sub(top + bar_h);
        // A body taller than its room gives up its last row to the scroll marker.
        let view_h = if page.body.lines.len() > room {
            room.saturating_sub(1)
        } else {
            room
        };
        let over = page.body.lines.len().saturating_sub(view_h);
        self.main_scroll = self.main_scroll.min(over);
        hits.extend(blit(
            buf,
            (mx, top, mw, view_h),
            &page.body,
            self.main_scroll,
        ));
        if over > 0 && room > 0 {
            let marker = if self.main_scroll < over {
                "▼ more"
            } else {
                "▲ more"
            };
            let x = (mx + mw).saturating_sub(text::width(marker));
            buf.set_line(
                x as u16,
                (top + view_h) as u16,
                &styled(marker, accent()),
                6,
            );
        }
        self.body_area = (mx, top, mw, view_h);
        // The page's header, body and bar move the highlight and scroll as one region.
        self.regions.push((Area::Main, (mx, 0, mw, body_h)));
        if bar_h > 0 {
            let y = body_h.saturating_sub(bar_h);
            buf.set_line(
                mx as u16,
                y as u16,
                &styled("─".repeat(mw), dim()),
                mw as u16,
            );
            hits.extend(blit(buf, (mx, y + 1, mw, bar_h - 1), &page.bar, 0));
        }

        // Status line.
        let (status, buttons) = self.status_line(w);
        buf.set_line(0, (h - 1) as u16, &status, w as u16);
        hits.extend(
            buttons
                .into_iter()
                .map(|hit| layout::Hit { y: h - 1, ..hit }),
        );

        // Whatever is in front takes every click.
        if let Some(view) = self.dialog_view() {
            hits = self.draw_dialog(buf, view);
        } else if let Some(menu) = self.menu.clone() {
            hits = self.draw_menu(buf, &menu);
        }
        self.hits = hits;
        self.after_draw(buf);
    }

    /// Follows the pointer and draws the highlighted control in reverse video.
    fn after_draw(&mut self, buf: &mut Buffer) {
        self.hovered();
        self.sync_focus();
        if std::mem::take(&mut self.enter_page) {
            // The first control of the page's body, if it has one.
            let (rx, ry, rw, rh) = self.body_area;
            self.focus = self
                .hits
                .iter()
                .find(|h| h.x >= rx && h.x < rx + rw && h.y >= ry && h.y < ry + rh)
                .map(|h| h.action.clone());
        }
        let area = buf.area;
        if let Some(h) = self.focus_hit()
            && h.y < usize::from(area.height)
        {
            let end = (h.x + h.w).min(usize::from(area.width));
            for x in h.x..end {
                let cell = &mut buf[(x as u16, h.y as u16)];
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            }
        }
    }

    /// The sidebar's scrolling part, its pinned footer, and the line of the selected row.
    fn sidebar(&self, w: usize) -> (Layout, Layout, Option<usize>) {
        let mut l = Layout::new(w);
        let mut selected = None;
        let shown = self.shown();
        l.line(styled(" Cordial", title()));
        l.row();
        let row = |text: &str, chosen: bool| {
            let look = if chosen {
                layout::selected().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            pad(
                Line::from(vec![span(" ", look), span(text.to_owned(), look)]),
                w,
            )
            .style(look)
        };
        if shown == Page::Overview {
            selected = Some(l.lines.len());
        }
        l.control(
            row("Overview", shown == Page::Overview),
            Action::Open(Page::Overview),
        );

        let can_add = !self.ready_adapters().is_empty();
        let connectable: Vec<_> = if can_add {
            Vec::new()
        } else {
            self.adapters
                .iter()
                .filter(|a| a.conn == Conn::Disconnected)
                .collect()
        };
        if !self.devices.is_empty() || can_add || !connectable.is_empty() {
            l.row();
            l.line(styled(" DEVICES", dim().add_modifier(Modifier::BOLD)));
        }
        for d in &self.devices {
            let page = d.page();
            let chosen = shown == page;
            if chosen {
                selected = Some(l.lines.len());
            }
            let base = if chosen {
                layout::selected()
            } else {
                Style::new()
            };
            let dot = if d.connected() {
                span("● ", layout::inherit(ok(), base))
            } else {
                span("○ ", layout::inherit(dim(), base))
            };
            let mut right = Line::default();
            if let Some(b) = &d.battery {
                let mut text = b.percent.map(|p| format!("{p}%")).unwrap_or_default();
                if b.charging == Some(true) {
                    text.push_str(" ↯");
                }
                let look = match () {
                    _ if b.low() => err(),
                    _ if b.stale() => dim(),
                    _ => Style::new(),
                };
                right = Line::from(vec![
                    span(text.trim().to_owned(), layout::inherit(look, base)),
                    span(" ", base),
                ]);
            }
            let first = spread(
                Line::from(vec![
                    span(" ", base),
                    dot,
                    span(d.name.clone(), layout::inherit(Style::new(), base)),
                ]),
                right,
                w,
            )
            .style(base);
            let second = pad(
                Line::from(vec![span(
                    format!("   {}", words::device_status(&d.d)),
                    layout::inherit(dim(), base),
                )]),
                w,
            )
            .style(base);
            l.control2(first, second, Action::Open(page));
        }
        if can_add && self.devices.is_empty() {
            l.control(row("+ Add Device", false), Action::AddDevice);
        }
        for a in connectable {
            l.control(
                row(&format!("Connect {}", a.name), false),
                Action::ConnectAdapter(a.id.clone()),
            );
        }

        l.row();
        l.line(styled(" ADAPTERS", dim().add_modifier(Modifier::BOLD)));
        for a in &self.adapters {
            let page = Page::Adapter(a.id.clone());
            let chosen = shown == page;
            if chosen {
                selected = Some(l.lines.len());
            }
            let base = if chosen {
                layout::selected()
            } else {
                Style::new()
            };
            let (status, attention) = a.status_text();
            let name_look = if a.connected() { Style::new() } else { dim() };
            let dot = match a.conn {
                Conn::Connected if a.ready => span("● ", layout::inherit(ok(), base)),
                _ => span("○ ", layout::inherit(dim(), base)),
            };
            let first = pad(
                Line::from(vec![
                    span(" ", base),
                    dot,
                    span(a.name.clone(), layout::inherit(name_look, base)),
                ]),
                w,
            )
            .style(base);
            let look = if attention { warn() } else { dim() };
            let second = pad(
                Line::from(vec![span(
                    format!("   {status}"),
                    layout::inherit(look, base),
                )]),
                w,
            )
            .style(base);
            l.control2(first, second, Action::Open(page));
        }
        if self.adapters.is_empty() {
            l.control(row("Refresh Adapters", false), Action::RefreshAdapters);
        }

        let mut pinned = Layout::new(w);
        if !self.devices.is_empty() {
            pinned.button("+ Add Device", Action::AddDevice, Tone::Primary);
            pinned.indent();
        }
        (l, pinned, selected)
    }

    /// The shown page.
    fn page_view(&mut self, w: usize) -> PageView {
        match self.shown() {
            Page::Overview => self.overview(w),
            Page::Adapter(id) => self.adapter_page(&id, w),
            Page::Device(a, id) => self.device_page(&a, id, w),
        }
    }

    /// The Overview: counts, what needs attention, the connected devices and the adapters.
    fn overview(&self, w: usize) -> PageView {
        let mut v = PageView::new(styled("Overview", bold()), w);
        let b = &mut v.body;
        if self.adapters.is_empty() {
            b.row();
            if self.listed {
                b.line(styled("No Adapter Found", bold()));
            }
            return v;
        }
        let connected = self.devices.iter().filter(|d| d.connected()).count();
        let adapters = self.adapters.iter().filter(|a| a.connected()).count();
        b.line(Line::from(vec![
            span(connected.to_string(), title()),
            span(" Connected   ", dim()),
            span(self.devices.len().to_string(), bold()),
            span(" Paired   ", dim()),
            span(adapters.to_string(), bold()),
            span(" Adapters", dim()),
        ]));

        let alerts = self.alerts();
        if !alerts.is_empty() {
            b.section("Needs Attention");
            for alert in alerts {
                let first = Line::from(vec![
                    span("! ", err().add_modifier(Modifier::BOLD)),
                    span(alert.name.clone(), bold()),
                ]);
                let first = spread(first, styled("›", dim()), w);
                let second = styled(format!("  {}", alert.detail), err());
                b.control2(first, second, Action::Show(alert.page.clone()));
            }
        }

        let several = adapters > 1;
        if connected > 0 {
            b.section("Connected");
            for d in self.devices.iter().filter(|d| d.connected()) {
                let mut right = Line::default();
                if let Some(p) = d.battery.as_ref().and_then(|b| b.percent) {
                    let low = d.low();
                    let stale = d.battery.as_ref().is_some_and(|b| b.stale());
                    let look = match () {
                        _ if low => err(),
                        _ if stale => dim(),
                        _ => Style::new(),
                    };
                    let filled = (p.clamp(0, 100) as usize * 10).div_ceil(100);
                    right = Line::from(vec![
                        span("█".repeat(filled), if low { err() } else { accent() }),
                        span("░".repeat(10 - filled), dim()),
                        span(format!(" {p:>3}%"), look),
                    ]);
                }
                let first = spread(
                    Line::from(vec![span("● ", ok()), span(d.name.clone(), bold())]),
                    right,
                    w,
                );
                if several {
                    let adapter = self
                        .adapter(&d.adapter)
                        .map(|a| a.name.clone())
                        .unwrap_or_default();
                    b.control2(
                        first,
                        styled(format!("  {adapter}"), dim()),
                        Action::Show(d.page()),
                    );
                } else {
                    b.control(first, Action::Show(d.page()));
                }
            }
        }

        b.section("Adapters");
        for a in &self.adapters {
            let (status, attention) = a.status_text();
            let mut right = Line::default();
            if a.connected() {
                let paired = self.devices.iter().filter(|d| d.adapter == a.id).count();
                let on = self
                    .devices
                    .iter()
                    .filter(|d| d.adapter == a.id && d.connected())
                    .count();
                right = styled(format!("{paired} paired · {on} connected ›"), dim());
            } else {
                right = join(right, styled("›", dim()));
            }
            let name_look = if a.connected() { bold() } else { dim() };
            let first = spread(styled(a.name.clone(), name_look), right, w);
            let look = if attention { warn() } else { dim() };
            b.control2(
                first,
                styled(format!("  {status}"), look),
                Action::Show(Page::Adapter(a.id.clone())),
            );
        }
        v
    }

    /// The bottom line: a failure or the most pressing alert, then the keys.
    /// The status line and its Help and Quit buttons, whose hits are on row 0.
    fn status_line(&self, w: usize) -> (Styled, Vec<layout::Hit>) {
        let mut right = Layout::new(w);
        if self.dialog.is_some() || self.menu.is_some() {
            right.line(styled("esc close ", dim()));
        } else {
            right.button("? Help", Action::Help, Tone::Normal);
            right.button("q Quit", Action::Quit, Tone::Normal);
            right.lines[0].spans.push(span(" ", Style::new()));
        }
        let alerts = self.alerts();
        let left = if let Some((text, _)) = &self.toast {
            Line::from(vec![span(" ", Style::new()), span(text.clone(), err())])
        } else if let Some(alert) = alerts.first() {
            let count = alerts.len();
            let mut spans = vec![
                span(" ! ", err().add_modifier(Modifier::BOLD)),
                span(alert.title.clone(), err().add_modifier(Modifier::BOLD)),
            ];
            if count > 1 {
                spans.push(span(format!(" (+{} more)", count - 1), err()));
            }
            Line::from(spans)
        } else {
            Line::default()
        };
        let right_line = right.lines.swap_remove(0);
        let x = w.saturating_sub(line_width(&right_line));
        let hits = right
            .hits
            .into_iter()
            .map(|hit| layout::Hit {
                x: hit.x + x,
                ..hit
            })
            .filter(|hit| hit.x + hit.w <= w)
            .collect();
        (spread(left, right_line, w), hits)
    }

    /// Draws a dialog box in the middle, dimming what is behind it, and returns its hits.
    fn draw_dialog(&mut self, buf: &mut Buffer, view: DialogView) -> Vec<layout::Hit> {
        for cell in buf.content.iter_mut() {
            cell.set_style(Style::reset().add_modifier(Modifier::DIM));
        }
        let (w, h) = (self.width, self.height);
        let bw = (view.body.width + 4).min(w.saturating_sub(2)).max(20);
        let inner = bw - 4;
        let buttons = view.buttons.lines.len();
        let max_body = h.saturating_sub(4 + buttons + usize::from(buttons > 0));
        let body_h = view.body.lines.len().min(max_body);
        let bh = body_h + 2 + buttons + usize::from(buttons > 0);
        let (x, y) = ((w - bw) / 2, h.saturating_sub(bh) / 2);
        let rect = Rect::new(x as u16, y as u16, bw as u16, bh as u16).intersection(buf.area);
        buf.set_style(rect, Style::reset());
        for row in 0..bh {
            let line = " ".repeat(bw);
            buf.set_line(x as u16, (y + row) as u16, &Line::from(line), bw as u16);
        }
        let heading = format!(" {} ", text::display(&view.title));
        let top = join(
            join(styled("╭─", dim()), styled(heading.clone(), title())),
            styled(
                format!(
                    "{}╮",
                    "─".repeat(bw.saturating_sub(3 + text::width(&heading)))
                ),
                dim(),
            ),
        );
        buf.set_line(x as u16, y as u16, &truncate(top, bw), bw as u16);
        for row in 1..bh - 1 {
            buf.set_line(x as u16, (y + row) as u16, &styled("│", dim()), 1);
            buf.set_line(
                (x + bw - 1) as u16,
                (y + row) as u16,
                &styled("│", dim()),
                1,
            );
        }
        let bottom = styled(format!("╰{}╯", "─".repeat(bw - 2)), dim());
        buf.set_line(x as u16, (y + bh - 1) as u16, &bottom, bw as u16);
        let over = view.body.lines.len().saturating_sub(body_h);
        self.dialog_scroll = self.dialog_scroll.min(over);
        let mut hits = blit(
            buf,
            (x + 2, y + 1, inner, body_h),
            &view.body,
            self.dialog_scroll,
        );
        if over > 0 {
            let marker = if self.dialog_scroll < over {
                "▼"
            } else {
                "▲"
            };
            buf.set_line(
                (x + bw - 1) as u16,
                (y + body_h) as u16,
                &styled(marker, accent()),
                1,
            );
        }
        // The body and buttons move the highlight as one region; the body scrolls.
        self.regions
            .push((Area::Dialog, (x + 1, y + 1, bw - 2, bh - 2)));
        if buttons > 0 {
            hits.extend(blit(
                buf,
                (x + 2, y + 2 + body_h, inner, buttons),
                &view.buttons,
                0,
            ));
        }
        hits
    }

    /// Draws a menu at its place and returns its hits.
    fn draw_menu(&mut self, buf: &mut Buffer, menu: &super::Menu) -> Vec<layout::Hit> {
        let inner = menu
            .items
            .iter()
            .map(|(label, _)| text::width(label))
            .max()
            .unwrap_or(0)
            + 2;
        // The menu shows as many items as fit on screen, below the control that opened it, or
        // above it when there is more room there.
        let rows = self.height.saturating_sub(1);
        let at = menu.y.min(rows);
        let below = rows - at;
        let above = at.saturating_sub(1);
        let fits = |room: usize| menu.items.len().min(room.saturating_sub(2));
        let (shown, y) = if fits(below) >= menu.items.len() || below >= above {
            (fits(below), at)
        } else {
            let n = fits(above);
            (n, at.saturating_sub(1 + n + 2))
        };
        if shown == 0 {
            return Vec::new();
        }
        let (bw, bh) = (inner + 2, shown + 2);
        let x = menu.x.min(self.width.saturating_sub(bw));
        let rect = Rect::new(x as u16, y as u16, bw as u16, bh as u16).intersection(buf.area);
        buf.set_style(rect, Style::reset());
        buf.set_line(
            x as u16,
            y as u16,
            &styled(format!("╭{}╮", "─".repeat(inner)), dim()),
            bw as u16,
        );
        let mut hits = Vec::new();
        for (i, (label, action)) in menu.items.iter().take(shown).enumerate() {
            let look = if action.is_some() {
                Style::new()
            } else {
                dim()
            };
            let line = Line::from(vec![
                span("│", dim()),
                span(layout::pad_str(&format!(" {label}"), inner), look),
                span("│", dim()),
            ]);
            buf.set_line(x as u16, (y + 1 + i) as u16, &line, bw as u16);
            if let Some(action) = action {
                hits.push(layout::Hit {
                    x: x + 1,
                    y: y + 1 + i,
                    w: inner,
                    action: action.clone(),
                });
            }
        }
        buf.set_line(
            x as u16,
            (y + bh - 1) as u16,
            &styled(format!("╰{}╯", "─".repeat(inner)), dim()),
            bw as u16,
        );
        hits
    }

    /// The open dialog, built for this frame.
    fn dialog_view(&mut self) -> Option<DialogView> {
        let dialog = self.dialog.clone()?;
        let w = (self.width.saturating_sub(8)).min(64);
        Some(match &dialog {
            Dialog::Help => self.help_dialog(w),
            Dialog::Rename(id) => self.rename_dialog(id, w),
            Dialog::Forget(a, id) => self.forget_dialog(a, *id, w),
            Dialog::ProfileName(a, copy) => self.profile_name_dialog(a, *copy, w),
            Dialog::ProfileDelete(a, id) => self.profile_delete_dialog(a, *id, w),
            Dialog::Pick {
                adapter,
                purpose,
                chosen,
            } => self.pick_dialog(adapter, *purpose, *chosen, w),
            Dialog::SaveAdapter(id) => self.reconnect_dialog(id, w),
            Dialog::AddDevice => self.add_dialog(w),
            Dialog::Bootloader(_) => self.bootloader_dialog(w),
            Dialog::Files(id) => self.files_dialog(id, (self.width.saturating_sub(8)).min(90)),
            Dialog::Replace(_, target) => self.replace_dialog(target, w),
        })
    }

    /// The keys, as the Help dialog lists them.
    fn help_dialog(&self, w: usize) -> DialogView {
        let mut body = Layout::new(w);
        for (keys, what) in [
            ("Mouse", "Click buttons, options and rows."),
            (
                "Tab",
                "Move between controls; Shift+Tab moves back. Enter activates the highlighted control, and Enter or Space chooses a highlighted On or Off option such as Show Unnamed Devices.",
            ),
            ("↑ ↓", "Select in the sidebar, or move between controls."),
            ("← →", "Change the highlighted setting, or switch tabs."),
            ("PgUp PgDn", "Scroll the page."),
            ("Ctrl+S", "Press the Save on screen."),
            ("Ctrl+N", "Add a device."),
            ("F5", "Refresh adapters."),
            ("m", "Open the selected adapter's menu."),
            ("Esc", "Close a menu or dialog, or cancel a pairing prompt."),
            ("? q", "Show this help, or quit."),
            (
                "Quit",
                "Anything this app started, such as a scan, stops. Saved devices stay paired and keep working.",
            ),
        ] {
            body.fact(keys, what, layout::plain());
        }
        let mut buttons = Layout::new(w);
        buttons.button_right("Close", Action::Cancel, Tone::Primary);
        DialogView {
            title: "Help".into(),
            body,
            buttons,
        }
    }
}

/// A spinner followed by words, as a busy indicator.
pub(super) fn busy(text: &str) -> Styled {
    Line::from(vec![
        span(format!("{} ", spinner()), accent()),
        span(text.to_owned(), dim()),
    ])
}

/// Adds a button, or the same label without a target while unavailable.
pub(super) fn button_if(b: &mut Layout, label: &str, action: Action, tone: Tone, enabled: bool) {
    if enabled {
        b.button(label, action, tone);
    } else {
        b.disabled(label);
    }
}

/// A row title, marked when a change to it is staged.
pub(super) fn staged_label(label: &str, staged: bool) -> Styled {
    let mut l = Line::from(span(label.to_owned(), Style::new()));
    if staged {
        l.spans.push(span(" ✎ Changed", accent()));
    }
    l
}
