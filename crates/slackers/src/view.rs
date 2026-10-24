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
use ratatui_textarea::TextArea;
use slackers_model::{
    Activity, Author, Block as TextBlock, Broadcast, Conversation, ConversationKind, Emoji, Inline,
    Message, RichText, Style as TextStyle, Timeline, Ts,
};
use slackers_slack::Connection;
use unicode_width::UnicodeWidthStr as _;

use crate::app::{App, Drawn, Focus, Mode, Phase, Place, Workspace};
use crate::emoji::Drawable;
use crate::icons;
use crate::layout::{Piece, wrap};
use crate::pane::{self, Pane, Settled};
use crate::pictures::PictureState;
use crate::theme::{
    self, ACCENT, ALERT, CODE, DIM, LINK, MATCH, MENTION, MENTION_ME, READ, SELECTED,
    SELECTED_UNFOCUSED,
};

const SIDEBAR_WIDTH: u16 = 30;

/// The narrowest message area, in cells, that shows a Thread beside its Conversation rather than instead of it.
const SPLIT_AT: u16 = 120;

/// Cells left of a Message's content, holding its author's avatar: two cells of picture and a space.
const GUTTER: u16 = 3;

/// Messages by the same author less than this far apart share one header.
const GROUP_WITHIN: u64 = 5 * 60 * 1_000_000;

/// Draws `app` on `frame`, and reports what only drawing can tell.
pub(crate) fn draw(frame: &mut Frame<'_>, app: &App) -> Drawn {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    let mut drawn = Drawn::default();
    match &app.phase {
        Phase::Ready(workspace) => {
            let messages = if app.sidebar_shown {
                let [sidebar, messages] =
                    Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)])
                        .areas(main);
                draw_sidebar(frame, sidebar, app, workspace);
                messages
            } else {
                main
            };
            draw_conversation(frame, messages, app, workspace, &mut drawn);
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
    frame.render_widget(
        Paragraph::new(Line::from(workspace_title(workspace).to_vec())),
        header,
    );

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
                (true, Focus::Sidebar) => line.patch_style(SELECTED),
                (true, Focus::Timeline | Focus::Thread) | (false, _) if is_open => {
                    line.patch_style(SELECTED_UNFOCUSED)
                }
                _ => line,
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

/// The Workspace's icon and name: the sidebar's title, and the start of the leftmost pane's while the sidebar hides.
fn workspace_title(workspace: &Workspace) -> [Span<'static>; 2] {
    [
        Span::styled(icons::WORKSPACE, ACCENT),
        Span::styled(
            workspace.name.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
    ]
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
        READ
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

/// Draws the open Conversation, with its open Thread beside it, or instead of it when there's no room for both.
fn draw_conversation(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    workspace: &Workspace,
    drawn: &mut Drawn,
) {
    let Some(open) = &app.open else { return };
    let Some(conversation) = workspace.conversations.iter().find(|c| &c.id == open) else {
        return;
    };
    let zone = TimeZone::system();
    let (conversation_area, thread_area) = match app.open_thread {
        None => (Some(area), None),
        Some(_) if area.width >= SPLIT_AT => {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(area);
            let rule = Block::new().borders(Borders::LEFT).border_style(DIM);
            let right_inner = rule.inner(right);
            frame.render_widget(rule, right);
            (Some(left), Some(right_inner))
        }
        Some(_) => (None, Some(area)),
    };
    let (icon, name) = label(app, conversation);
    // With the sidebar hidden, its title leads the leftmost pane's, breadcrumb-style.
    let crumbs = |leftmost: bool| {
        let mut spans = vec![Span::raw(" ")];
        if leftmost && !app.sidebar_shown {
            spans.extend(workspace_title(workspace));
            spans.push(Span::styled(format!(" {}", icons::CRUMB), DIM));
        }
        spans
    };

    if let Some(area) = conversation_area {
        let focused = app.focus == Focus::Timeline;
        let mut title = crumbs(true);
        title.extend([
            Span::styled(icon, if focused { ACCENT } else { DIM }),
            Span::styled(name.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
        ]);
        let title = Line::from(title);
        let view = PaneView {
            title,
            timeline: workspace.timelines.get(open),
            pane: &app.conversation_pane,
            place: Place {
                conversation: open.clone(),
                thread: None,
            },
            focused,
        };
        drawn.conversation = draw_pane(frame, area, &view, app, workspace, &zone, drawn);
    }
    if let (Some(area), Some(parent)) = (thread_area, app.open_thread) {
        let focused = app.focus == Focus::Thread;
        let mut title = crumbs(conversation_area.is_none());
        title.extend([
            Span::styled(icons::THREAD, if focused { ACCENT } else { DIM }),
            Span::styled("thread", Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" in {icon}{name}"), DIM),
        ]);
        let title = Line::from(title);
        let view = PaneView {
            title,
            timeline: workspace.threads.get(&(open.clone(), parent)),
            pane: &app.thread_pane,
            place: Place {
                conversation: open.clone(),
                thread: Some(parent),
            },
            focused,
        };
        drawn.thread = draw_pane(frame, area, &view, app, workspace, &zone, drawn);
    }
}

/// One pane of Messages to draw: a Conversation's, or a Thread's.
struct PaneView<'a> {
    title: Line<'static>,
    /// Its Messages; `None` while they load.
    timeline: Option<&'a Timeline>,
    pane: &'a Pane,
    /// Where its drafts and Messages go; a Thread when [`Place::thread`] is set.
    place: Place,
    /// Whether keys go to it.
    focused: bool,
}

/// One Message laid out, or something else shown among Messages.
struct Rendered {
    /// The Message; `None` for what can't be selected, such as a Message still being sent.
    ts: Option<Ts>,
    lines: Vec<Line<'static>>,
    /// How many of `lines` come before the Message itself, such as a day's rule; they aren't highlighted.
    leading: usize,
    /// Pictures it would show, had they been loaded, by URL.
    wanted: Vec<String>,
    /// When one of its pictures next changes frame, as animation time.
    next_change: Option<Duration>,
}

/// Draws a pane of Messages, its title and its draft, and returns where it settled; `None` while its Messages load.
fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    view: &PaneView<'_>,
    app: &App,
    workspace: &Workspace,
    zone: &TimeZone,
    drawn: &mut Drawn,
) -> Option<Settled> {
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    frame.render_widget(Paragraph::new(view.title.clone()), header);
    let body = body.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let inserting = app.mode == Mode::Insert && app.writing_place().as_ref() == Some(&view.place);
    let body = draw_composer(frame, body, app.drafts.get(&view.place), inserting);
    let Some(timeline) = view.timeline else {
        let loading = Line::from(vec![
            Span::styled(icons::LOADING, ACCENT),
            Span::styled("loading…", DIM),
        ]);
        frame.render_widget(Paragraph::new(loading), body);
        return None;
    };

    let rendered = lay_out(app, workspace, timeline, &view.place, body.width, zone);
    let blocks: Vec<pane::Block> = rendered
        .iter()
        .map(|r| pane::Block {
            ts: r.ts,
            lines: r.lines.len(),
        })
        .collect();
    let height = usize::from(body.height);
    let settled = view.pane.settle(&blocks, height);

    // Show the slice the scroll position puts at the bottom, with the selected Message tinted.
    let total: usize = blocks.iter().map(|block| block.lines).sum();
    let end = total - settled.scroll;
    let start = end.saturating_sub(height);
    let selected = settled.cursor.or_else(|| timeline.newest());
    let mut visible = Vec::with_capacity(height);
    // The selected Message's rows, within `visible`.
    let mut selected_rows = 0..0;
    let mut offset = 0;
    for block in rendered {
        let range = offset..offset + block.lines.len();
        offset = range.end;
        if range.end <= start || range.start >= end {
            continue;
        }
        drawn.wanted_pictures.extend(block.wanted);
        drawn.next_change = earliest(drawn.next_change, block.next_change);
        if block.ts.is_some() && block.ts == selected {
            let message = (range.start + block.leading).max(start)..range.end.min(end);
            selected_rows = message.start - start..message.end - start;
        }
        let skip = start.saturating_sub(range.start);
        let take = end.min(range.end) - range.start.max(start);
        visible.extend(block.lines.into_iter().skip(skip).take(take));
    }
    // Anchor to the bottom, like a chat.
    let shown = u16::try_from(visible.len())
        .unwrap_or(body.height)
        .min(body.height);
    let top = body.y + body.height - shown;
    frame.render_widget(
        Paragraph::new(visible),
        Rect {
            y: top,
            height: shown,
            ..body
        },
    );
    // Tint the selected Message as a block, across the whole pane, not just where its lines have text.
    if !selected_rows.is_empty() {
        let row = |index: usize| top + u16::try_from(index).unwrap_or(u16::MAX);
        let rows = Rect {
            y: row(selected_rows.start),
            height: row(selected_rows.end) - row(selected_rows.start),
            ..area
        };
        frame.buffer_mut().set_style(
            rows,
            if view.focused {
                SELECTED
            } else {
                SELECTED_UNFOCUSED
            },
        );
    }
    Some(settled)
}

/// Lays out `timeline`'s Messages for `place`, `width` cells wide, followed by those still being sent there. A
/// Thread's parent comes first, then a rule counting its Replies.
fn lay_out(
    app: &App,
    workspace: &Workspace,
    timeline: &Timeline,
    place: &Place,
    width: u16,
    zone: &TimeZone,
) -> Vec<Rendered> {
    let in_thread = place.thread.is_some();
    let replies = timeline.messages().len().saturating_sub(1);
    let mut rendered = Vec::with_capacity(timeline.messages().len() + 1);
    let mut previous: Option<&Message> = None;
    for (index, message) in timeline.messages().enumerate() {
        let day = day(message.ts, zone);
        let new_day = previous.is_none_or(|p| day != self::day(p.ts, zone));
        let first_reply = in_thread && index == 1;
        let mut lines = Vec::new();
        if new_day && let Some(day) = day {
            lines.push(
                Line::styled(format!("─── {} ───", day.strftime("%a %d %b %Y")), DIM)
                    .alignment(Alignment::Center),
            );
        }
        if first_reply {
            let noun = if replies == 1 { "reply" } else { "replies" };
            lines.push(
                Line::styled(format!("─── {replies} {noun} ───"), DIM).alignment(Alignment::Center),
            );
        }
        let leading = lines.len();
        let grouped = !new_day
            && !first_reply
            && previous.is_some_and(|p| {
                p.author == message.author
                    && message.ts.as_micros().saturating_sub(p.ts.as_micros()) < GROUP_WITHIN
            });
        let mut renderer = Renderer::new(app, workspace, width, zone);
        renderer.in_thread = in_thread;
        lines.extend(renderer.message(message, grouped));
        rendered.push(Rendered {
            ts: Some(message.ts),
            lines,
            leading,
            wanted: renderer.wanted,
            next_change: renderer.next_change,
        });
        previous = Some(message);
    }
    if in_thread && replies == 0 {
        let hint = Line::styled("no replies yet · i to reply", DIM).alignment(Alignment::Center);
        rendered.push(Rendered {
            ts: None,
            lines: vec![hint],
            leading: 0,
            wanted: Vec::new(),
            next_change: None,
        });
    }
    // Messages on their way follow, dimmed, under the user's name.
    for outgoing in app
        .outbox
        .iter()
        .filter(|outgoing| &outgoing.place == place)
    {
        let mut renderer = Renderer::new(app, workspace, width, zone);
        let lines = renderer.sending(&outgoing.text);
        rendered.push(Rendered {
            ts: None,
            lines,
            leading: 0,
            wanted: renderer.wanted,
            next_change: renderer.next_change,
        });
    }
    rendered
}

/// Draws `draft` at the bottom of `area`: editable when `inserting`, dimmed otherwise, and not at all when there's
/// none, or it's empty and not being written. Returns what's left of `area` above it.
fn draw_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    draft: Option<&TextArea<'static>>,
    inserting: bool,
) -> Rect {
    let Some(draft) = draft.filter(|draft| inserting || !draft.is_empty()) else {
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
///
/// A Message hangs off its author's avatar: the avatar and a space fill the [`GUTTER`] on the header line, and
/// everything else is indented by it.
struct Renderer<'a> {
    app: &'a App,
    workspace: &'a Workspace,
    /// Room for content, right of the gutter, in cells.
    width: u16,
    zone: &'a TimeZone,
    /// Pictures it would show, had they been loaded, by URL.
    wanted: Vec<String>,
    /// When one of its pictures next changes frame, as animation time.
    next_change: Option<Duration>,
    now: Duration,
    /// Whether it lays out a Thread, whose pane counts the Replies itself.
    in_thread: bool,
}

impl<'a> Renderer<'a> {
    /// A renderer for lines `width` cells wide, gutter included.
    fn new(app: &'a App, workspace: &'a Workspace, width: u16, zone: &'a TimeZone) -> Self {
        Self {
            app,
            workspace,
            width: width.saturating_sub(GUTTER),
            zone,
            wanted: Vec::new(),
            next_change: None,
            now: app.animation_time(),
            in_thread: false,
        }
    }

    /// `message`'s lines. A `grouped` one follows another by the same author: it gets no header, just a faint dot
    /// in the gutter where it starts.
    fn message(&mut self, message: &Message, grouped: bool) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if !grouped {
            let mut header = self.header(&message.author);
            header.push(Span::styled(
                format!("  {}", time(message.ts, self.zone)),
                DIM,
            ));
            if message.edited {
                header.push(Span::styled(format!(" {}", icons::EDITED), DIM));
            }
            lines.push(Line::from(header));
        }
        let mut content = self.text(&message.content);
        if grouped
            && message.edited
            && let Some(last) = content.last_mut()
        {
            last.spans
                .push(Span::styled(format!(" {}", icons::EDITED), DIM));
        }
        for file in &message.files {
            content.push(Line::from(vec![
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
            content.extend(wrap(pieces, self.width).into_iter().map(Line::from));
        }
        if let Some(thread) = message.thread.filter(|_| !self.in_thread) {
            content.push(Line::from(vec![
                Span::styled(icons::THREAD, ACCENT),
                Span::styled(thread.replies.to_string(), ACCENT),
                Span::styled(format!("  {}", time(thread.latest, self.zone)), DIM),
            ]));
        }
        lines.extend(content.into_iter().enumerate().map(|(index, mut line)| {
            let gutter = if grouped && index == 0 { " · " } else { "   " };
            line.spans.insert(0, Span::styled(gutter, DIM));
            line
        }));
        lines
    }

    /// The lines of `text`, posted by the user and not yet confirmed: dimmed, under a header saying so.
    fn sending(&mut self, text: &str) -> Vec<Line<'static>> {
        let mut header = self.header(&Author::User(self.workspace.me.clone()));
        header.extend([
            Span::raw("  "),
            Span::styled(icons::SENDING, DIM),
            Span::styled("sending", DIM),
        ]);
        let mut lines = vec![Line::from(header)];
        let pieces = vec![Piece::Text(text.to_owned(), DIM)];
        lines.extend(wrap(pieces, self.width).into_iter().map(|mut spans| {
            spans.insert(0, Span::raw("   "));
            Line::from(spans)
        }));
        lines
    }

    /// The start of a header line: `author`'s avatar in the gutter, then their name.
    fn header(&mut self, author: &Author) -> Vec<Span<'static>> {
        let (name, color) = match author {
            Author::User(id) => (self.app.user_name(id).to_owned(), author_color(id.as_str())),
            Author::Bot { name, .. } => (name.clone(), theme::BOT),
        };
        vec![
            self.avatar(author, color),
            Span::raw(" "),
            Span::styled(name, Style::new().fg(color).add_modifier(Modifier::BOLD)),
        ]
    }

    /// `author`'s avatar, two cells wide: their picture once loaded, an icon in `color` until then or without one.
    fn avatar(&mut self, author: &Author, color: Color) -> Span<'static> {
        let icon = match author {
            Author::User(_) => icons::AVATAR,
            Author::Bot { .. } => icons::BOT,
        };
        let fallback = Span::styled(icon, Style::new().fg(color));
        let Some(url) = self.app.avatar(author) else {
            return fallback;
        };
        match self.app.pictures.state(url) {
            PictureState::Ready(picture) => {
                self.next_change = earliest(self.next_change, picture.next_change(self.now));
                picture.row(0, self.now)
            }
            PictureState::Awaiting { requested } => {
                if !requested {
                    self.wanted.push(url.to_owned());
                }
                fallback
            }
            PictureState::Failed => fallback,
        }
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
        match self.app.emojis.drawable(emoji, &self.app.pictures) {
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
        (Phase::Ready(_), Some(Connection::Live)) => (icons::LIVE, "live".to_owned(), theme::LIVE),
        (Phase::Ready(_), Some(Connection::Reconnecting { attempt })) => (
            icons::OFFLINE,
            format!("reconnecting ({attempt})"),
            theme::RECONNECTING,
        ),
    };
    let mut left = Vec::new();
    if app.mode == Mode::Insert {
        let insert = theme::INSERT;
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
        (_, Focus::Sidebar) => {
            "j/k select · ⏎ open · / search · \\ hide · i write · tab messages · :q quit"
        }
        (_, Focus::Timeline) => {
            "j/k message · ⏎ thread · ^e/^y line · ^d/^u page · g/G · i write · \\ sidebar · :q quit"
        }
        (_, Focus::Thread) => {
            "j/k message · ^e/^y line · i reply · esc close · \\ sidebar · :q quit"
        }
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
    let hash = id.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    theme::AUTHORS[hash % theme::AUTHORS.len()]
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
        render(app).0
    }

    /// Draws `app` and returns the screen, row by row, and what the draw reported.
    fn render(app: &App) -> (Vec<String>, Drawn) {
        let (rows, drawn, _) = render_sized(app, WIDTH);
        (rows, drawn)
    }

    /// Draws `app` `width` cells wide, and returns the screen, what the draw reported, and the cells' styles.
    fn render_sized(app: &App, width: u16) -> (Vec<String>, Drawn, ratatui::buffer::Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
        let mut drawn = Drawn::default();
        terminal.draw(|frame| drawn = draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..HEIGHT)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        (rows, drawn, buffer)
    }

    /// The one Message `reading_general` holds, by `U2`.
    fn reading_general_message() -> Message {
        Message {
            ts: Ts::from_micros(1_700_000_000_000_000),
            author: Author::User(UserId::new("U2")),
            content: RichText::plain("the only message"),
            reply_to: None,
            thread: None,
            reactions: Vec::new(),
            files: Vec::new(),
            edited: false,
        }
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
        let page = HistoryPage {
            messages: vec![reading_general_message()],
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
    fn messages_hang_off_their_authors_avatar() {
        let mut app = reading_general();
        app.update(Event::Slack(Update::User(slackers_slack::User {
            id: UserId::new("U2"),
            name: "Ana".into(),
            avatar: Some("https://a/ana.png".into()),
        })));
        let follow_up = Message {
            ts: Ts::from_micros(1_700_000_060_000_000),
            content: RichText::plain("a minute later"),
            ..reading_general_message()
        };
        app.update(Event::Slack(Update::Live(
            slackers_slack::LiveEvent::Posted {
                conversation: ConversationId::new("C1"),
                message: follow_up,
            },
        )));

        let (rows, drawn) = render(&app);
        // The sidebar is 30 cells wide, and the message pane has a margin of 1.
        let pane = |row: &String| row.chars().skip(31).collect::<String>();
        let header = rows
            .iter()
            .map(pane)
            .find(|row| row.contains("Ana"))
            .unwrap();
        // Avatar (an icon and its space, until the picture loads), a space, then the name, aligned with the text.
        assert!(
            header.starts_with(&format!("{} Ana", icons::AVATAR)),
            "{header:?}"
        );
        let body = rows
            .iter()
            .map(pane)
            .find(|row| row.contains("the only message"))
            .unwrap();
        assert!(body.starts_with("   the only message"), "{body:?}");
        let grouped = rows
            .iter()
            .map(pane)
            .find(|row| row.contains("a minute later"))
            .unwrap();
        assert!(grouped.starts_with(" · a minute later"), "{grouped:?}");
        assert_eq!(drawn.wanted_pictures, ["https://a/ana.png"]);
    }

    /// `reading_general` with the only Message's Thread open, holding one Reply.
    fn reading_a_thread() -> App {
        let mut app = reading_general();
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let parent = reading_general_message();
        let reply = Message {
            ts: Ts::from_micros(1_700_000_100_000_000),
            content: RichText::plain("the reply"),
            reply_to: Some(parent.ts),
            ..reading_general_message()
        };
        app.update(Event::Slack(Update::Replies {
            conversation: ConversationId::new("C1"),
            parent: parent.ts,
            result: Ok(vec![parent, reply]),
        }));
        app
    }

    #[test]
    fn a_wide_screen_shows_the_thread_beside_its_conversation() {
        let app = reading_a_thread();
        let (rows, drawn, buffer) = render_sized(&app, 200);
        assert!(drawn.conversation.is_some() && drawn.thread.is_some());
        assert!(
            rows[0].contains("general") && rows[0].contains("thread in"),
            "{:?}",
            rows[0]
        );
        assert!(rows.iter().any(|row| row.contains("─── 1 reply ───")));
        let parents: usize = rows
            .iter()
            .map(|row| row.matches("the only message").count())
            .sum();
        assert_eq!(parents, 2, "the parent shows in both panes");

        // The thread pane has focus: its selected Message, the newest Reply, is tinted brighter.
        let (x, y) = rows
            .iter()
            .enumerate()
            .find_map(|(y, row)| {
                row.find("the reply")
                    .map(|byte| (row[..byte].chars().count(), y))
            })
            .unwrap();
        let (x, y) = (u16::try_from(x).unwrap(), u16::try_from(y).unwrap());
        let selected = SELECTED.bg.unwrap();
        assert_eq!(buffer[(x, y)].bg, selected);
        // As a block: from the pane's left edge to its right one, well past the end of the text.
        assert_eq!(buffer[(x - 4, y)].bg, selected, "the gutter and margin");
        assert_eq!(buffer[(199, y)].bg, selected, "the pane's right edge");
        // The Conversation's pane, without focus, shows its selection (the parent) more quietly.
        let in_left_pane = |row: &String| {
            row.chars()
                .skip(30)
                .take(90)
                .collect::<String>()
                .contains("the only message")
        };
        let parent_row = rows.iter().position(in_left_pane).unwrap();
        let in_conversation = u16::try_from(parent_row).unwrap();
        assert_eq!(
            buffer[(40, in_conversation)].bg,
            SELECTED_UNFOCUSED.bg.unwrap()
        );
    }

    #[test]
    fn a_narrow_screen_shows_the_thread_instead() {
        let app = reading_a_thread();
        let (rows, drawn, _) = render_sized(&app, 100);
        assert!(drawn.conversation.is_none() && drawn.thread.is_some());
        assert!(rows[0].contains("thread in"));
        assert!(rows.iter().any(|row| row.contains("the reply")));
    }

    #[test]
    fn a_hidden_sidebar_leaves_the_whole_width_to_the_messages() {
        let mut app = reading_general();
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(
            screen(&app)[0].starts_with(icons::WORKSPACE),
            "the sidebar's title comes first"
        );

        type_text(&mut app, "\\");
        let rows = screen(&app);
        let breadcrumb = format!(
            " {}Test {}{}general",
            icons::WORKSPACE,
            icons::CRUMB,
            icons::CHANNEL
        );
        assert!(
            rows[0].starts_with(&breadcrumb),
            "the sidebar's title leads: {:?}",
            rows[0]
        );
        let body = rows
            .iter()
            .find(|row| row.contains("the only message"))
            .unwrap();
        assert!(
            body.starts_with("    the only message"),
            "a margin, then the gutter: {body:?}"
        );
    }

    #[test]
    fn a_thread_covering_its_conversation_carries_the_breadcrumb() {
        let mut app = reading_a_thread();
        app.focus = Focus::Sidebar;
        type_text(&mut app, "\\");
        let (rows, _, _) = render_sized(&app, 100);
        let breadcrumb = format!(
            " {}Test {}{}thread",
            icons::WORKSPACE,
            icons::CRUMB,
            icons::THREAD
        );
        assert!(rows[0].starts_with(&breadcrumb), "{:?}", rows[0]);

        let (rows, _, _) = render_sized(&app, 200);
        let only_leftmost = rows[0].contains("thread in") && rows[0].matches("Test").count() == 1;
        assert!(only_leftmost, "{:?}", rows[0]);
    }

    #[test]
    fn command_mode_takes_the_status_line() {
        let mut app = reading_general();
        type_text(&mut app, ":q");
        assert!(screen(&app).last().unwrap().starts_with(":q"));
    }
}
