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

use crate::app::{App, Drawn, Focus, Phase, Workspace};
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

    // Keep the selection in view.
    let rows = usize::from(list.height);
    let first = (app.selected + 1).saturating_sub(rows);
    let lines: Vec<Line<'_>> = workspace
        .conversations
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(index, conversation)| {
            let activity = workspace.activity.get(&conversation.id);
            let line = sidebar_row(app, conversation, activity, list.width);
            let is_open = app.open.as_ref() == Some(&conversation.id);
            match (index == app.selected, app.focus) {
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
    match &conversation.kind {
        ConversationKind::Channel { private: false } => (icons::CHANNEL, &conversation.name),
        ConversationKind::Channel { private: true } => (icons::PRIVATE_CHANNEL, &conversation.name),
        ConversationKind::Dm { with } => (icons::DM, app.user_name(with)),
        ConversationKind::GroupDm => (icons::GROUP_DM, &conversation.name),
    }
}

/// One sidebar entry: an icon for its kind, its name, bold when unread, and on the right its Mentions, or a dot
/// when only unread.
fn sidebar_row<'a>(
    app: &'a App,
    conversation: &'a Conversation,
    activity: Option<&Activity>,
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
    let room = usize::from(width).saturating_sub(2 + badge.width() + 1);
    let mut shown = String::new();
    for c in name.chars() {
        if shown.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > room {
            shown.pop();
            shown.push('…');
            break;
        }
        shown.push(c);
    }
    let padding = room.saturating_sub(shown.width()) + 1;
    Line::from(vec![
        Span::styled(icon, if unread { ACCENT } else { DIM }),
        Span::styled(shown, name_style),
        Span::raw(" ".repeat(padding)),
        Span::styled(badge, badge_style),
    ])
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
    let mut left = vec![Span::styled(icon, style), Span::styled(state, style)];
    if let Some(notice) = &app.notice {
        left.extend([
            Span::raw("  "),
            Span::styled(icons::ALERT, ALERT),
            Span::styled(notice.clone(), ALERT),
        ]);
    }
    let hints = match app.focus {
        Focus::Sidebar => "j/k select · ⏎ open · tab messages · q quit",
        Focus::Timeline => "k/j scroll · ^u/^d page · g/G oldest/newest · tab sidebar · q quit",
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
