//! Drawing the app: the sidebar, the open Timeline and the status bar.

use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use slackers_model::{
    Activity, Author, Block as TextBlock, Broadcast, Conversation, ConversationKind, Emoji, Inline,
    Message, RichText, Style as TextStyle, Ts,
};
use slackers_slack::Connection;
use unicode_width::UnicodeWidthStr as _;

use crate::app::{App, Drawn, Focus, Mode, Phase, Workspace};
use crate::emoji::Drawable;
use crate::icons;
use crate::layout::{Piece, wrap};

const SIDEBAR_WIDTH: u16 = 30;

/// Messages by the same author less than this far apart share one header.
const GROUP_WITHIN: u64 = 5 * 60 * 1_000_000;

const DIM: Style = Style::new().fg(Color::DarkGray);
const ACCENT: Style = Style::new().fg(Color::Cyan);
const MENTION: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const MENTION_ME: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
const LINK: Style = Style::new()
    .fg(Color::Blue)
    .add_modifier(Modifier::UNDERLINED);
const CODE: Style = Style::new().fg(Color::LightRed);
const ALERT: Style = Style::new().fg(Color::Red);
const MATCH: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);

/// Draws `app` on `frame`, and reports what only drawing can tell.
pub(crate) fn draw(frame: &mut Frame<'_>, app: &App) -> Drawn {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    let mut drawn = Drawn::default();
    match &app.phase {
        Phase::Ready(workspace) => {
            let [sidebar, timeline] =
                Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)])
                    .areas(main);
            draw_sidebar(frame, sidebar, app, workspace);
            draw_timeline(frame, timeline, app, workspace, &mut drawn);
        }
        Phase::SigningIn => {
            let text = Line::from(vec![
                Span::styled(icons::LOADING, ACCENT),
                Span::raw(format!("signing in to {}…", app.label)),
            ]);
            frame.render_widget(
                Paragraph::new(text).alignment(Alignment::Center),
                middle(main),
            );
        }
        Phase::Failed(reason) => {
            let text = vec![
                Line::from(vec![
                    Span::styled(icons::ALERT, ALERT),
                    Span::raw(format!("couldn't sign in to {}", app.label)),
                ]),
                Line::styled(reason.clone(), DIM),
            ];
            frame.render_widget(
                Paragraph::new(text)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true }),
                middle(main),
            );
        }
    }
    draw_status(frame, status, app);
    drawn
}

/// A few rows in the vertical middle of `area`.
fn middle(area: Rect) -> Rect {
    let height = area.height.min(3);
    Rect {
        y: area.y + (area.height - height) / 2,
        height,
        ..area
    }
}

fn draw_sidebar(frame: &mut Frame<'_>, area: Rect, app: &App, workspace: &Workspace) {
    let block = Block::new().borders(Borders::RIGHT).border_style(DIM);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [header, list] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    let title = Line::from(vec![
        Span::styled(icons::WORKSPACE, ACCENT),
        Span::styled(
            workspace.name.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), header);

    // What to list, in which order, where the cursor is, and which characters matched a search.
    let (entries, cursor): (Vec<(usize, &[usize])>, usize) = match &app.mode {
        Mode::Search(search) => (
            search
                .hits
                .iter()
                .map(|hit| (hit.index, hit.positions.as_slice()))
                .collect(),
            search.selected,
        ),
        _ => (
            (0..workspace.conversations.len())
                .map(|index| (index, &[][..]))
                .collect(),
            app.selected,
        ),
    };
    // Keep the cursor in view.
    let rows = usize::from(list.height);
    let first = (cursor + 1).saturating_sub(rows);
    let lines: Vec<Line<'_>> = entries
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(row, &(index, matched))| {
            let conversation = &workspace.conversations[index];
            let activity = workspace.activity.get(&conversation.id);
            let line = sidebar_row(app, conversation, activity, matched, list.width);
            let is_open = app.open.as_ref() == Some(&conversation.id);
            match (row == cursor, app.focus) {
                (true, Focus::Sidebar) => {
                    line.patch_style(Style::new().add_modifier(Modifier::REVERSED))
                }
                (true, Focus::Timeline) | (false, _) if is_open => {
                    line.patch_style(Style::new().bg(Color::Indexed(236)))
                }
                _ => line,
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

/// The icon for `conversation`'s kind, and what to call it.
fn label<'a>(app: &'a App, conversation: &'a Conversation) -> (&'static str, &'a str) {
    let icon = match &conversation.kind {
        ConversationKind::Channel { private: false } => icons::CHANNEL,
        ConversationKind::Channel { private: true } => icons::PRIVATE_CHANNEL,
        ConversationKind::Dm { .. } => icons::DM,
        ConversationKind::GroupDm => icons::GROUP_DM,
    };
    (icon, app.conversation_name(conversation))
}

/// One sidebar entry: an icon for its kind, its name (bold when unread, with `matched` characters highlighted),
/// and on the right its Mentions, or a dot when only unread.
fn sidebar_row<'a>(
    app: &'a App,
    conversation: &'a Conversation,
    activity: Option<&Activity>,
    matched: &[usize],
    width: u16,
) -> Line<'a> {
    let (icon, name) = label(app, conversation);
    let unread = activity.is_some_and(Activity::has_unread);
    let mentions = activity.map_or(0, Activity::mentions);
    let (badge, badge_style) = match (mentions, unread) {
        (0, false) => (String::new(), DIM),
        (0, true) => (icons::UNREAD.to_owned(), ACCENT),
        (mentions, _) => (format!("{}{mentions}", icons::MENTION), MENTION_ME),
    };
    let name_style = if unread {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        DIM
    };

    // The name, cut to fit with an ellipsis, in runs of matched and unmatched characters.
    let room = usize::from(width).saturating_sub(2 + badge.width() + 1);
    let limit = if name.width() > room {
        room.saturating_sub(1)
    } else {
        room
    };
    let mut spans = vec![Span::styled(icon, if unread { ACCENT } else { DIM })];
    let (mut run, mut run_matched, mut used) = (String::new(), false, 0);
    for (index, c) in name.chars().enumerate() {
        let cells = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cells > limit {
            break;
        }
        let is_match = matched.binary_search(&index).is_ok();
        if is_match != run_matched && !run.is_empty() {
            let style = if run_matched {
                name_style.patch(MATCH)
            } else {
                name_style
            };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_matched = is_match;
        run.push(c);
        used += cells;
    }
    let style = if run_matched {
        name_style.patch(MATCH)
    } else {
        name_style
    };
    spans.push(Span::styled(run, style));
    if limit < room {
        spans.push(Span::styled("…", name_style));
        used += 1;
    }
    spans.push(Span::raw(" ".repeat(room.saturating_sub(used) + 1)));
    spans.push(Span::styled(badge, badge_style));
    Line::from(spans)
}

fn draw_timeline(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    workspace: &Workspace,
    drawn: &mut Drawn,
) {
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let body = body.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let Some(open) = &app.open else { return };
    let conversation = workspace.conversations.iter().find(|c| &c.id == open);
    if let Some(conversation) = conversation {
        let (icon, name) = label(app, conversation);
        let style = if app.focus == Focus::Timeline {
            ACCENT
        } else {
            DIM
        };
        let title = Line::from(vec![
            Span::raw(" "),
            Span::styled(icon, style),
            Span::styled(name.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
        ]);
        frame.render_widget(Paragraph::new(title), header);
    }
    let body = draw_composer(frame, body, app);
    let Some(timeline) = workspace.timelines.get(open) else {
        let loading = Line::from(vec![
            Span::styled(icons::LOADING, ACCENT),
            Span::styled("loading…", DIM),
        ]);
        frame.render_widget(Paragraph::new(loading), body);
        return;
    };

    // Lay out every Message, then show the slice that the scroll position puts at the bottom.
    let zone = TimeZone::system();
    let mut rendered = Vec::with_capacity(timeline.messages().len());
    let mut previous: Option<&Message> = None;
    for message in timeline.messages() {
        let day = day(message.ts, &zone);
        let new_day = previous.is_none_or(|p| day != self::day(p.ts, &zone));
        let mut renderer = Renderer::new(app, workspace, body.width, &zone);
        let mut lines = Vec::new();
        if new_day && let Some(day) = day {
            lines.push(
                Line::styled(format!("─── {} ───", day.strftime("%a %d %b %Y")), DIM)
                    .alignment(Alignment::Center),
            );
        }
        let grouped = !new_day
            && previous.is_some_and(|p| {
                p.author == message.author
                    && message.ts.as_micros().saturating_sub(p.ts.as_micros()) < GROUP_WITHIN
            });
        lines.extend(renderer.message(message, grouped));
        rendered.push((lines, renderer.wanted, renderer.next_change));
        previous = Some(message);
    }
    // Messages on their way follow, dimmed, under the user's name.
    for outgoing in app
        .outbox
        .iter()
        .filter(|outgoing| &outgoing.conversation == open)
    {
        let mut lines = vec![Line::from(vec![
            Span::styled(
                app.user_name(&workspace.me).to_owned(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(icons::SENDING, DIM),
            Span::styled("sending", DIM),
        ])];
        let pieces = vec![Piece::Text(outgoing.text.clone(), DIM)];
        lines.extend(wrap(pieces, body.width).into_iter().map(Line::from));
        rendered.push((lines, Vec::new(), None));
    }

    let height = usize::from(body.height);
    let total: usize = rendered.iter().map(|(lines, _, _)| lines.len()).sum();
    drawn.max_scroll = total.saturating_sub(height);
    drawn.page = height;
    let end = total - app.scroll.min(drawn.max_scroll);
    let start = end.saturating_sub(height);
    let mut visible = Vec::with_capacity(height);
    let mut offset = 0;
    for (lines, wanted, next_change) in rendered {
        let range = offset..offset + lines.len();
        offset = range.end;
        if range.end <= start || range.start >= end {
            continue;
        }
        drawn.wanted_pictures.extend(wanted);
        drawn.next_change = earliest(drawn.next_change, next_change);
        let skip = start.saturating_sub(range.start);
        let take = end.min(range.end) - range.start.max(start);
        visible.extend(lines.into_iter().skip(skip).take(take));
    }
    // Anchor to the bottom, like a chat.
    let shown = u16::try_from(visible.len())
        .unwrap_or(body.height)
        .min(body.height);
    let area = Rect {
        y: body.y + body.height - shown,
        height: shown,
        ..body
    };
    frame.render_widget(Paragraph::new(visible), area);
}

/// Draws the open Conversation's draft at the bottom of `area`: editable in Insert mode, dimmed otherwise, and not
/// at all when there's none. Returns what's left of `area` above it.
fn draw_composer(frame: &mut Frame<'_>, area: Rect, app: &App) -> Rect {
    let inserting = app.mode == Mode::Insert;
    let Some(draft) = app.draft().filter(|draft| inserting || !draft.is_empty()) else {
        return area;
    };
    let lines = u16::try_from(draft.lines().len()).unwrap_or(u16::MAX);
    let height = lines.saturating_add(1).clamp(2, (area.height / 3).max(2));
    let [above, composer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(height)]).areas(area);
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(if inserting { ACCENT } else { DIM });
    let inner = block.inner(composer);
    frame.render_widget(block, composer);
    if inserting {
        frame.render_widget(draft, inner);
    } else {
        let text: Vec<Line<'_>> = draft
            .lines()
            .iter()
            .map(|line| Line::styled(line.as_str(), DIM))
            .collect();
        frame.render_widget(Paragraph::new(text), inner);
    }
    above
}

/// Lays out one Message, collecting the pictures it wants and when its animations next change.
struct Renderer<'a> {
    app: &'a App,
    workspace: &'a Workspace,
    width: u16,
    zone: &'a TimeZone,
    /// Pictures it would show, had they been loaded, by URL.
    wanted: Vec<String>,
    /// When one of its pictures next changes frame, as animation time.
    next_change: Option<Duration>,
    now: Duration,
}

impl<'a> Renderer<'a> {
    fn new(app: &'a App, workspace: &'a Workspace, width: u16, zone: &'a TimeZone) -> Self {
        Self {
            app,
            workspace,
            width,
            zone,
            wanted: Vec::new(),
            next_change: None,
            now: app.animation_time(),
        }
    }

    fn message(&mut self, message: &Message, grouped: bool) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if !grouped {
            let (name, color) = match &message.author {
                Author::User(id) => (self.app.user_name(id).to_owned(), author_color(id.as_str())),
                Author::Bot { name } => (name.clone(), Color::Gray),
            };
            let mut header = vec![
                Span::styled(name, Style::new().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {}", time(message.ts, self.zone)), DIM),
            ];
            if message.edited {
                header.push(Span::styled(format!(" {}", icons::EDITED), DIM));
            }
            lines.push(Line::from(header));
        }
        lines.extend(self.text(&message.content));
        if grouped
            && message.edited
            && let Some(last) = lines.last_mut()
        {
            last.spans
                .push(Span::styled(format!(" {}", icons::EDITED), DIM));
        }
        for file in &message.files {
            lines.push(Line::from(vec![
                Span::styled(icons::FILE, DIM),
                Span::styled(file.name.clone(), DIM),
            ]));
        }
        if !message.reactions.is_empty() {
            let mut pieces = Vec::new();
            for reaction in &message.reactions {
                pieces.push(self.emoji(&reaction.emoji));
                let style = if reaction.mine { MENTION } else { DIM };
                pieces.push(Piece::Text(format!(" {}  ", reaction.count), style));
            }
            lines.extend(wrap(pieces, self.width).into_iter().map(Line::from));
        }
        if let Some(thread) = message.thread {
            lines.push(Line::from(vec![
                Span::styled(icons::THREAD, ACCENT),
                Span::styled(thread.replies.to_string(), ACCENT),
                Span::styled(format!("  {}", time(thread.latest, self.zone)), DIM),
            ]));
        }
        lines
    }

    fn text(&mut self, text: &RichText) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for block in &text.blocks {
            match block {
                TextBlock::Paragraph(inlines) => {
                    let pieces = self.pieces(inlines);
                    lines.extend(wrap(pieces, self.width).into_iter().map(Line::from));
                }
                TextBlock::Quote(inlines) => {
                    let pieces = self.pieces(inlines);
                    lines.extend(self.prefixed(pieces, Span::styled("▎ ", DIM), "  "));
                }
                TextBlock::Preformatted(code) => {
                    lines.extend(code.lines().map(|line| {
                        Line::from(vec![
                            Span::styled("▏ ", DIM),
                            Span::styled(line.to_owned(), CODE),
                        ])
                    }));
                }
                TextBlock::List { ordered, items } => {
                    for (index, item) in items.iter().enumerate() {
                        let marker = if *ordered {
                            format!("{}. ", index + 1)
                        } else {
                            "• ".to_owned()
                        };
                        let indent = " ".repeat(marker.width());
                        let pieces = self.pieces(item);
                        lines.extend(self.prefixed(pieces, Span::styled(marker, DIM), &indent));
                    }
                }
            }
        }
        lines
    }

    /// `pieces` wrapped to leave room for `first` before the first line and `rest` (as wide) before the others.
    fn prefixed(&self, pieces: Vec<Piece>, first: Span<'static>, rest: &str) -> Vec<Line<'static>> {
        let room = self
            .width
            .saturating_sub(u16::try_from(first.width()).unwrap_or(0));
        wrap(pieces, room)
            .into_iter()
            .enumerate()
            .map(|(index, mut spans)| {
                let prefix = if index == 0 {
                    first.clone()
                } else {
                    Span::raw(rest.to_owned())
                };
                spans.insert(0, prefix);
                Line::from(spans)
            })
            .collect()
    }

    fn pieces(&mut self, inlines: &[Inline]) -> Vec<Piece> {
        inlines
            .iter()
            .map(|inline| match inline {
                Inline::Text { text, style } => Piece::Text(text.clone(), text_style(*style)),
                Inline::Emoji(emoji) => self.emoji(emoji),
                Inline::User(id) => {
                    let style = if *id == self.workspace.me {
                        MENTION_ME
                    } else {
                        MENTION
                    };
                    Piece::Text(format!("@{}", self.app.user_name(id)), style)
                }
                Inline::Channel(id) => {
                    let known = self.workspace.conversations.iter().find(|c| &c.id == id);
                    Piece::Text(
                        format!("#{}", known.map_or(id.as_str(), |c| c.name.as_str())),
                        LINK,
                    )
                }
                Inline::Broadcast(broadcast) => Piece::Text(
                    match broadcast {
                        Broadcast::Here => "@here",
                        Broadcast::Channel => "@channel",
                        Broadcast::Everyone => "@everyone",
                    }
                    .to_owned(),
                    MENTION_ME,
                ),
                Inline::UserGroup(_) => Piece::Text("@group".to_owned(), MENTION),
                Inline::Link { url, label } => {
                    Piece::Text(label.clone().unwrap_or_else(|| url.clone()), LINK)
                }
            })
            .collect()
    }

    fn emoji(&mut self, emoji: &Emoji) -> Piece {
        match self.app.emojis.drawable(emoji) {
            Drawable::Text(text) => Piece::Atom(
                Span::raw(text.to_owned()),
                u16::try_from(text.width().max(1)).unwrap_or(2),
            ),
            Drawable::Picture(picture) => {
                self.next_change = earliest(self.next_change, picture.next_change(self.now));
                Piece::Atom(picture.row(0, self.now), picture.cells().cols)
            }
            Drawable::Awaiting { url, requested } => {
                if !requested {
                    self.wanted.push(url.to_owned());
                }
                Piece::Text(format!(":{}:", emoji.name), DIM)
            }
            Drawable::Name => Piece::Text(format!(":{}:", emoji.name), Style::new()),
        }
    }
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // Command and Search modes take the line, as in vim.
    let cursor = Span::styled(" ", Style::new().add_modifier(Modifier::REVERSED));
    match &app.mode {
        Mode::Command(line) => {
            let command = Line::from(vec![Span::raw(format!(":{line}")), cursor]);
            frame.render_widget(Paragraph::new(command), area);
            return;
        }
        Mode::Search(search) => {
            let query = Line::from(vec![Span::raw(format!("/{}", search.query)), cursor]);
            let total = app.workspace().map_or(0, |w| w.conversations.len());
            let hints = format!(
                "{} of {total} · ⏎ open · ^n/^p next/previous · esc cancel",
                search.hits.len()
            );
            frame.render_widget(Paragraph::new(query), area);
            frame.render_widget(
                Paragraph::new(Line::styled(hints, DIM)).alignment(Alignment::Right),
                area,
            );
            return;
        }
        Mode::Normal | Mode::Insert => {}
    }
    let (icon, state, style) = match (&app.phase, app.connection) {
        (Phase::SigningIn, _) | (Phase::Ready(_), None) => {
            (icons::LOADING, "connecting".to_owned(), ACCENT)
        }
        (Phase::Failed(_), _) => (icons::ALERT, "signed out".to_owned(), ALERT),
        (Phase::Ready(_), Some(Connection::Live)) => (
            icons::LIVE,
            "live".to_owned(),
            Style::new().fg(Color::Green),
        ),
        (Phase::Ready(_), Some(Connection::Reconnecting { attempt })) => (
            icons::OFFLINE,
            format!("reconnecting ({attempt})"),
            Style::new().fg(Color::Yellow),
        ),
    };
    let mut left = Vec::new();
    if app.mode == Mode::Insert {
        let insert = ACCENT.add_modifier(Modifier::BOLD);
        left.extend([
            Span::styled(icons::INSERT, insert),
            Span::styled("INSERT  ", insert),
        ]);
    }
    left.extend([Span::styled(icon, style), Span::styled(state, style)]);
    if let Some(notice) = &app.notice {
        left.extend([
            Span::raw("  "),
            Span::styled(icons::ALERT, ALERT),
            Span::styled(notice.clone(), ALERT),
        ]);
    }
    let hints = match (&app.mode, app.focus) {
        (Mode::Insert, _) => "⏎ send · ⇧⏎ new line · ^x^e $EDITOR · esc normal",
        (_, Focus::Sidebar) => "j/k select · ⏎ open · / search · i write · tab messages · :q quit",
        (_, Focus::Timeline) => "k/j scroll · ^u/^d page · g/G oldest/newest · i write · :q quit",
    };
    frame.render_widget(Paragraph::new(Line::from(left)), area);
    frame.render_widget(
        Paragraph::new(Line::styled(hints, DIM)).alignment(Alignment::Right),
        area,
    );
}

fn text_style(style: TextStyle) -> Style {
    let mut out = Style::new();
    for (flag, modifier) in [
        (TextStyle::BOLD, Modifier::BOLD),
        (TextStyle::ITALIC, Modifier::ITALIC),
        (TextStyle::STRIKE, Modifier::CROSSED_OUT),
    ] {
        if style.contains(flag) {
            out = out.add_modifier(modifier);
        }
    }
    if style.contains(TextStyle::CODE) {
        out.patch(CODE)
    } else {
        out
    }
}

/// A colour for an author, the same every time.
fn author_color(id: &str) -> Color {
    const PALETTE: [Color; 6] = [
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::LightRed,
    ];
    let hash = id.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    PALETTE[hash % PALETTE.len()]
}

fn zoned(ts: Ts, zone: &TimeZone) -> Option<jiff::Zoned> {
    let micros = i64::try_from(ts.as_micros()).ok()?;
    Some(
        Timestamp::from_microsecond(micros)
            .ok()?
            .to_zoned(zone.clone()),
    )
}

/// The local time of day `ts` was posted, as `HH:MM`.
fn time(ts: Ts, zone: &TimeZone) -> String {
    zoned(ts, zone)
        .map(|z| z.strftime("%H:%M").to_string())
        .unwrap_or_default()
}

/// The local date `ts` was posted on.
fn day(ts: Ts, zone: &TimeZone) -> Option<Date> {
    zoned(ts, zone).map(|z| z.date())
}

fn earliest(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use slackers_model::{Activity, ConversationId, Message, RichText, Ts, UserId, WorkspaceId};
    use slackers_slack::{HistoryPage, Snapshot, Update};

    use super::*;
    use crate::app::Event;

    const WIDTH: u16 = 80;
    const HEIGHT: u16 = 12;

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.update(Event::Terminal(TerminalEvent::Key(KeyEvent::new(
            code, modifiers,
        ))));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    /// Draws `app` and returns the screen, row by row.
    fn screen(app: &App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..HEIGHT)
            .map(|y| (0..WIDTH).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// An app with `#general` open, holding one Message.
    fn reading_general() -> App {
        let mut app = App::new("test".into());
        let general = Conversation {
            id: ConversationId::new("C1"),
            kind: ConversationKind::Channel { private: false },
            name: "general".into(),
        };
        let snapshot = Snapshot {
            workspace: WorkspaceId::new("T1"),
            name: "Test".into(),
            me: UserId::new("UME"),
            conversations: vec![(general, Activity::default())],
        };
        app.update(Event::Slack(Update::Ready(Box::new(snapshot))));
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let message = Message {
            ts: Ts::from_micros(1_700_000_000_000_000),
            author: Author::User(UserId::new("U2")),
            content: RichText::plain("the only message"),
            reply_to: None,
            thread: None,
            reactions: Vec::new(),
            files: Vec::new(),
            edited: false,
        };
        let page = HistoryPage {
            messages: vec![message],
            has_more: false,
        };
        app.update(Event::Slack(Update::History {
            conversation: ConversationId::new("C1"),
            result: Ok(page),
        }));
        app
    }

    #[test]
    fn insert_mode_opens_the_composer_under_the_messages_and_says_so() {
        let mut app = reading_general();
        assert!(!screen(&app).last().unwrap().contains("INSERT"));

        type_text(&mut app, "i");
        type_text(&mut app, "hello");
        press(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
        type_text(&mut app, "world");
        let rows = screen(&app);
        let row_of = |text: &str| rows.iter().position(|row| row.contains(text));
        let (message, hello, world) =
            (row_of("the only message"), row_of("hello"), row_of("world"));
        assert!(
            message < hello && hello.map(|h| h + 1) == world,
            "{rows:#?}"
        );
        assert!(rows.last().unwrap().contains("INSERT"), "{rows:#?}");

        // Back in Normal mode the draft stays, dimmed.
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let rows = screen(&app);
        assert!(rows.iter().any(|row| row.contains("hello")));
        assert!(!rows.last().unwrap().contains("INSERT"));
    }

    #[test]
    fn searching_lists_only_hits_and_shows_the_query() {
        let mut app = reading_general();
        type_text(&mut app, "/gnl");
        let rows = screen(&app);
        assert!(rows.iter().any(|row| row.contains("general")));
        let status = rows.last().unwrap();
        assert!(status.starts_with("/gnl"), "{status}");
        assert!(status.contains("1 of 1"), "{status}");

        type_text(&mut app, "x");
        let rows = screen(&app);
        assert!(
            !rows[1..rows.len() - 1]
                .iter()
                .any(|row| row.starts_with("\u{f4df}")),
            "{rows:#?}"
        );
        assert!(rows.last().unwrap().contains("0 of 1"));
    }

    #[test]
    fn command_mode_takes_the_status_line() {
        let mut app = reading_general();
        type_text(&mut app, ":q");
        assert!(screen(&app).last().unwrap().starts_with(":q"));
    }
}
