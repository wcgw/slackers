//! Drawing the app: the sidebar, the open Timeline and the status bar.

use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use slackers_gfx::{CellBox, Fit};
use slackers_model::{
    Activity, Author, Block as TextBlock, Broadcast, Conversation, ConversationKind, Emoji, File,
    Image, Inline, Message, Reaction, RichText, Style as TextStyle, Timeline, Ts,
};
use slackers_slack::Connection;
use unicode_width::UnicodeWidthStr as _;

use crate::app::{
    App, ConversationRef, Details, Drawn, Focus, LinkKind, Mode, Phase, Place, Suggested,
    Suggestions, WantedPicture, Workspace, WorkspaceKey,
};
use crate::draft::{Draft, Mentioned};
use crate::emoji::Drawable;
use crate::icons;
use crate::layout::{Piece, wrap};
use crate::pane::{self, Pane, Settled};
use crate::pictures::PictureState;
use crate::sidebar::{Item, Row, Section};
use crate::theme::{
    self, ACCENT, ALERT, CODE, DIM, LINK, MATCH, MENTION, MENTION_ME, READ, SELECTED,
    SELECTED_UNFOCUSED,
};

const SIDEBAR_WIDTH: u16 = 30;

/// Cells left of a sidebar row's name: a Workspace's icon or a fold marker, then an icon for what the row is.
const LEAD: usize = 4;

/// The narrowest message area, in cells, that shows a Thread beside its Conversation rather than instead of it.
const SPLIT_AT: u16 = 120;

/// Cells left of a Message's content, holding its author's avatar: two cells of picture and a space.
const GUTTER: u16 = 3;

/// The most room an image shared in a Message takes; one smaller than this shows at its own size.
const IMAGE: CellBox = CellBox { cols: 60, rows: 15 };

/// The widest the list of suggestions to complete a word with gets, in cells.
const SUGGESTIONS_WIDTH: u16 = 48;

/// The room an emoji gets in a list of suggestions, a space after it included.
const EMOJI_CELLS: u16 = 3;

/// The widest a Message's details get, in cells.
const DETAILS_WIDTH: u16 = 80;

/// Messages by the same author less than this far apart share one header.
const GROUP_WITHIN: u64 = 5 * 60 * 1_000_000;

/// Draws `app` on `frame`, and reports what only drawing can tell.
pub(crate) fn draw(frame: &mut Frame<'_>, app: &App) -> Drawn {
    let [main, status] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
    let mut drawn = Drawn::default();
    let messages = if app.sidebar_shown {
        let [sidebar, messages] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)])
                .areas(main);
        draw_sidebar(frame, sidebar, app, &mut drawn);
        messages
    } else {
        main
    };
    match &app.open {
        Some(open) => draw_conversation(frame, messages, app, open, &mut drawn),
        None => draw_sign_ins(frame, messages, app),
    }
    draw_status(frame, status, app);
    drawn
}

/// Says, in the middle of `area`, how signing in goes for each Workspace not signed in to yet.
fn draw_sign_ins(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines = Vec::new();
    for (_, sign_in) in app.workspaces.iter() {
        match &sign_in.phase {
            Phase::Ready(_) => {}
            Phase::SigningIn => lines.push(Line::from(vec![
                Span::styled(icons::LOADING, ACCENT),
                Span::raw(format!("signing in to {}…", sign_in.label)),
            ])),
            Phase::Failed(reason) => {
                lines.push(Line::from(vec![
                    Span::styled(icons::ALERT, ALERT),
                    Span::raw(format!("couldn't sign in to {}", sign_in.label)),
                ]));
                lines.push(Line::styled(reason.clone(), DIM));
            }
        }
    }
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        middle(area, height),
    );
}

/// `height` rows in the vertical middle of `area`, or all of it.
fn middle(area: Rect, height: u16) -> Rect {
    let height = area.height.min(height);
    Rect {
        y: area.y + (area.height - height) / 2,
        height,
        ..area
    }
}

/// Draws the sidebar: its rows, or while searching, the hits.
fn draw_sidebar(frame: &mut Frame<'_>, area: Rect, app: &App, drawn: &mut Drawn) {
    let block = Block::new().borders(Borders::RIGHT).border_style(DIM);
    let list = block.inner(area);
    frame.render_widget(block, area);
    let height = usize::from(list.height);
    // Keep the cursor in view.
    let first_shown = |cursor: usize| (cursor + 1).saturating_sub(height);
    let lines: Vec<Line<'static>> = match &app.mode {
        Mode::Search(search) => search
            .hits
            .iter()
            .enumerate()
            .skip(first_shown(search.selected))
            .take(height)
            .map(|(index, hit)| {
                let line = conversation_row(
                    app,
                    &hit.conversation,
                    true,
                    &hit.positions,
                    list.width,
                    drawn,
                );
                if index == search.selected {
                    line.patch_style(SELECTED)
                } else {
                    line
                }
            })
            .collect(),
        _ => {
            let cursor = app.sidebar.cursor();
            app.sidebar
                .rows()
                .iter()
                .enumerate()
                .skip(first_shown(cursor))
                .take(height)
                .map(|(index, row)| {
                    let (line, is_open) = match row {
                        Row::Header(section) => {
                            (header_row(app, section, list.width, drawn), false)
                        }
                        Row::Item {
                            section,
                            item: Item::Conversation(conversation),
                        } => {
                            // Only a Workspace's own section needs no mark of the Workspace on each row.
                            let marked = !matches!(section, Section::Workspace(_));
                            let line =
                                conversation_row(app, conversation, marked, &[], list.width, drawn);
                            (line, app.open.as_ref() == Some(conversation))
                        }
                        Row::Item {
                            item:
                                Item::Thread {
                                    conversation,
                                    parent,
                                },
                            ..
                        } => {
                            let line = thread_row(app, conversation, *parent, list.width, drawn);
                            let is_open = app.open.as_ref() == Some(conversation)
                                && app.open_thread == Some(*parent);
                            (line, is_open)
                        }
                    };
                    match (index == cursor, app.focus) {
                        (true, Focus::Sidebar) => line.patch_style(SELECTED),
                        (true, Focus::Timeline | Focus::Thread) | (false, _) if is_open => {
                            line.patch_style(SELECTED_UNFOCUSED)
                        }
                        _ => line,
                    }
                })
                .collect()
        }
    };
    frame.render_widget(Paragraph::new(lines), list);
}

/// What a sidebar row says about unread Messages: how many Mentions there are, or whether there are any at all.
#[derive(Clone, Copy, Debug, Default)]
struct Badge {
    unread: bool,
    mentions: u32,
}

impl Badge {
    fn of(activity: Option<&Activity>) -> Self {
        Self {
            unread: activity.is_some_and(Activity::has_unread),
            mentions: activity.map_or(0, Activity::mentions),
        }
    }

    /// `item`'s: a Conversation's Activity, or a Followed Thread's.
    fn of_item(app: &App, item: &Item) -> Self {
        match item {
            Item::Conversation(conversation) => Self::of(app.workspaces.activity(conversation)),
            Item::Thread {
                conversation,
                parent,
            } => Self::of(
                app.workspaces
                    .followed(conversation, *parent)
                    .map(|followed| &followed.activity),
            ),
        }
    }

    /// Both badges in one, as a section's header shows its rows'.
    fn plus(self, other: Self) -> Self {
        Self {
            unread: self.unread || other.unread,
            mentions: self.mentions.saturating_add(other.mentions),
        }
    }

    /// The Mentions with their icon, or a dot when there are only unread Messages, or nothing.
    fn span(self) -> Span<'static> {
        match (self.mentions, self.unread) {
            (0, false) => Span::raw(""),
            (0, true) => Span::styled(icons::UNREAD, ACCENT),
            (mentions, _) => Span::styled(format!("{}{mentions}", icons::MENTION), MENTION_ME),
        }
    }
}

/// A section's header: whether it's folded, its icon and name, and the combined badge of what it holds.
fn header_row(app: &App, section: &Section, width: u16, drawn: &mut Drawn) -> Line<'static> {
    let fold = if app.sidebar.is_folded(section) {
        icons::FOLDED
    } else {
        icons::UNFOLDED
    };
    let badge = app
        .sidebar
        .items_in(section)
        .iter()
        .map(|item| Badge::of_item(app, item))
        .fold(Badge::default(), Badge::plus);
    let (icon, name) = section_title(app, section, drawn);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    entry(
        vec![Span::styled(fold, DIM), icon],
        &name,
        bold,
        &[],
        badge,
        width,
    )
}

/// A section's icon and name: the Threads', a Virtual Workspace's, or a Workspace's, whose icon says how signing in to
/// it went.
fn section_title(app: &App, section: &Section, drawn: &mut Drawn) -> (Span<'static>, String) {
    match section {
        Section::Threads => (Span::styled(icons::THREAD, ACCENT), "Threads".to_owned()),
        Section::VirtualWorkspace(name) => {
            (Span::styled(icons::VIRTUAL_WORKSPACE, ACCENT), name.clone())
        }
        Section::Workspace(key) => {
            let Some(sign_in) = app.workspaces.sign_in(*key) else {
                return (Span::raw("  "), String::new());
            };
            match &sign_in.phase {
                Phase::SigningIn => (Span::styled(icons::LOADING, ACCENT), sign_in.label.clone()),
                Phase::Failed(_) => (Span::styled(icons::ALERT, ALERT), sign_in.label.clone()),
                Phase::Ready(workspace) => (
                    workspace_marker(app, *key, workspace, drawn),
                    workspace.name.clone(),
                ),
            }
        }
    }
}

/// A Conversation's row: when `marked`, its Workspace's icon; an icon for its kind; its name (bold when unread,
/// with the `matched` characters highlighted); and on the right its badge.
fn conversation_row(
    app: &App,
    conversation: &ConversationRef,
    marked: bool,
    matched: &[usize],
    width: u16,
    drawn: &mut Drawn,
) -> Line<'static> {
    let Some((workspace, found)) = app.workspaces.conversation(conversation) else {
        return Line::default();
    };
    let badge = Badge::of(workspace.activity.get(&conversation.id));
    let (icon, name) = label(workspace, found);
    let marker = if marked {
        workspace_marker(app, conversation.workspace, workspace, drawn)
    } else {
        Span::raw("  ")
    };
    let (icon_style, name_style) = if badge.unread {
        (ACCENT, Style::new().add_modifier(Modifier::BOLD))
    } else {
        (DIM, READ)
    };
    let lead = vec![marker, Span::styled(icon, icon_style)];
    entry(lead, name, name_style, matched, badge, width)
}

/// A Followed Thread's row: its Workspace's icon; a Thread icon; its Conversation's name and the gist of its parent
/// (bold when there are Replies to read); and on the right its badge.
fn thread_row(
    app: &App,
    conversation: &ConversationRef,
    parent: Ts,
    width: u16,
    drawn: &mut Drawn,
) -> Line<'static> {
    let Some((workspace, found)) = app.workspaces.conversation(conversation) else {
        return Line::default();
    };
    let followed = workspace.followed.get(&(conversation.id.clone(), parent));
    let badge = Badge::of(followed.map(|followed| &followed.activity));
    let (_, name) = label(workspace, found);
    let parent_message = followed
        .and_then(|followed| followed.parent.as_ref())
        .or_else(|| workspace.timelines.get(&conversation.id)?.get(parent));
    let text = match parent_message.map(|message| gist(workspace, &message.content)) {
        Some(gist) if !gist.is_empty() => format!("{name}: {gist}"),
        _ => name.to_owned(),
    };
    let (icon_style, name_style) = if badge.unread {
        (ACCENT, Style::new().add_modifier(Modifier::BOLD))
    } else {
        (DIM, READ)
    };
    let marker = workspace_marker(app, conversation.workspace, workspace, drawn);
    let lead = vec![marker, Span::styled(icons::THREAD, icon_style)];
    entry(lead, &text, name_style, &[], badge, width)
}

/// `text` as one line of plain words: people, Channels and emoji by name, links by their label.
fn gist(workspace: &Workspace, text: &RichText) -> String {
    let mut gist = String::new();
    for inline in text.inlines() {
        match inline {
            Inline::Text { text, .. } => gist.push_str(text),
            Inline::Emoji(emoji) => gist.push_str(&format!(":{}:", emoji.name)),
            Inline::User(id) => gist.push_str(&format!("@{}", workspace.user_name(id))),
            Inline::Channel(id) => {
                let name = workspace
                    .conversation(id)
                    .map_or(id.as_str(), |c| c.name.as_str());
                gist.push_str(&format!("#{name}"));
            }
            Inline::Broadcast(broadcast) => gist.push_str(match broadcast {
                Broadcast::Here => "@here",
                Broadcast::Channel => "@channel",
                Broadcast::Everyone => "@everyone",
            }),
            Inline::UserGroup(_) => gist.push_str("@group"),
            Inline::Link { url, label } => gist.push_str(label.as_deref().unwrap_or(url)),
        }
    }
    gist.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One sidebar row, `width` cells wide: `lead`, [`LEAD`] cells wide; `name` in `name_style`, its `matched`
/// characters highlighted, cut to fit with an ellipsis; and on the right, `badge`.
fn entry(
    mut lead: Vec<Span<'static>>,
    name: &str,
    name_style: Style,
    matched: &[usize],
    badge: Badge,
    width: u16,
) -> Line<'static> {
    let badge = badge.span();
    let room = usize::from(width).saturating_sub(LEAD + badge.width() + 1);
    let limit = if name.width() > room {
        room.saturating_sub(1)
    } else {
        room
    };
    let (spans, mut used) = highlighted(name, name_style, matched, limit);
    lead.extend(spans);
    if limit < room {
        lead.push(Span::styled("…", name_style));
        used += 1;
    }
    lead.push(Span::raw(" ".repeat(room.saturating_sub(used) + 1)));
    lead.push(badge);
    Line::from(lead)
}

/// As much of `text` in `style` as fits in `limit` cells, its `matched` characters (by char index, ascending)
/// highlighted; and how many cells that takes.
fn highlighted(
    text: &str,
    style: Style,
    matched: &[usize],
    limit: usize,
) -> (Vec<Span<'static>>, usize) {
    let styled = |matched: bool| if matched { style.patch(MATCH) } else { style };
    // Runs of matched and unmatched characters.
    let (mut spans, mut run, mut run_matched, mut used) = (Vec::new(), String::new(), false, 0);
    for (index, c) in text.chars().enumerate() {
        let cells = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cells > limit {
            break;
        }
        let is_match = matched.binary_search(&index).is_ok();
        if is_match != run_matched && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), styled(run_matched)));
        }
        run_matched = is_match;
        run.push(c);
        used += cells;
    }
    spans.push(Span::styled(run, styled(run_matched)));
    (spans, used)
}

/// The Workspace at `key`'s icon, two cells wide: its picture once loaded, else its initial.
fn workspace_marker(
    app: &App,
    key: WorkspaceKey,
    workspace: &Workspace,
    drawn: &mut Drawn,
) -> Span<'static> {
    let initial: String = workspace
        .name
        .chars()
        .next()
        .map(char::to_uppercase)
        .into_iter()
        .flatten()
        .collect();
    let color = stable_color(&workspace.name);
    let fallback = Span::styled(
        format!("{initial:<1} "),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    );
    let Some(url) = &workspace.icon else {
        return fallback;
    };
    match app.pictures.state(url) {
        PictureState::Ready(picture) => {
            let now = app.animation_time();
            drawn.next_change = earliest(drawn.next_change, picture.next_change(now));
            picture.row(0, now)
        }
        PictureState::Awaiting { requested } => {
            if !requested {
                drawn.wanted_pictures.push(WantedPicture {
                    workspace: key,
                    url: url.clone(),
                    fit: Fit::Emoji,
                });
            }
            fallback
        }
        PictureState::Failed => fallback,
    }
}

/// The icon for `conversation`'s kind, and what to call it.
fn label<'a>(workspace: &'a Workspace, conversation: &'a Conversation) -> (&'static str, &'a str) {
    let icon = match &conversation.kind {
        ConversationKind::Channel { private: false } => icons::CHANNEL,
        ConversationKind::Channel { private: true } => icons::PRIVATE_CHANNEL,
        ConversationKind::Dm { .. } => icons::DM,
        ConversationKind::GroupDm => icons::GROUP_DM,
    };
    (icon, workspace.conversation_name(conversation))
}

/// Draws the open Conversation, with its open Thread beside it, or instead of it when there's no room for both.
fn draw_conversation(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    open: &ConversationRef,
    drawn: &mut Drawn,
) {
    let Some((workspace, conversation)) = app.workspaces.conversation(open) else {
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
    let (icon, name) = label(workspace, conversation);
    // With the sidebar hidden, the section the Conversation is listed in leads the leftmost pane's title,
    // breadcrumb-style. The Workspace's icon says where the Conversation is from, unless that section does.
    let section = app
        .sidebar
        .section_of(open)
        .cloned()
        .unwrap_or(Section::Workspace(open.workspace));
    let mut crumb = Vec::new();
    if !app.sidebar_shown {
        let (section_icon, section_name) = section_title(app, &section, drawn);
        crumb.extend([
            section_icon,
            Span::styled(section_name, Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}", icons::CRUMB), DIM),
        ]);
    }
    let marker = (app.sidebar_shown || matches!(section, Section::VirtualWorkspace(_)))
        .then(|| workspace_marker(app, open.workspace, workspace, drawn));
    let crumbs = |leftmost: bool| {
        let mut spans = vec![Span::raw(" ")];
        if leftmost {
            spans.extend(crumb.iter().cloned());
        }
        spans
    };

    if let Some(area) = conversation_area {
        let focused = app.focus == Focus::Timeline;
        let mut title = crumbs(true);
        title.extend(marker.clone());
        title.extend([
            Span::styled(icon, if focused { ACCENT } else { DIM }),
            Span::styled(name.to_owned(), Style::new().add_modifier(Modifier::BOLD)),
        ]);
        let view = PaneView {
            title: Line::from(title),
            timeline: workspace.timelines.get(&open.id),
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
            Span::styled(" in ", DIM),
        ]);
        title.extend(marker);
        title.push(Span::styled(format!("{icon}{name}"), DIM));
        let view = PaneView {
            title: Line::from(title),
            timeline: workspace.threads.get(&(open.id.clone(), parent)),
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
    /// Pictures it would show, had they been loaded.
    wanted: Vec<WantedPicture>,
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
    let suggestions = app
        .suggestions
        .as_ref()
        .filter(|_| inserting)
        .map(|suggestions| {
            let key = view.place.conversation.workspace;
            (
                suggestions,
                Renderer::new(app, workspace, key, body.width, zone),
            )
        });
    let Some(timeline) = view.timeline else {
        let loading = Line::from(vec![
            Span::styled(icons::LOADING, ACCENT),
            Span::styled("loading…", DIM),
        ]);
        frame.render_widget(Paragraph::new(loading), body);
        if let Some((suggestions, renderer)) = suggestions {
            draw_suggestions(frame, body, suggestions, renderer, drawn);
        }
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
    if let Mode::Details(details) = &app.mode
        && details.place == view.place
    {
        // Out of the way of the Message they're about: above it when it's in the bottom half, else below.
        let middle = usize::from(top) + (selected_rows.start + selected_rows.end) / 2;
        let at_top = !selected_rows.is_empty()
            && middle >= usize::from(body.y) + usize::from(body.height) / 2;
        draw_details(frame, body, at_top, app, details, zone, drawn);
    }
    if let Some((suggestions, renderer)) = suggestions {
        draw_suggestions(frame, body, suggestions, renderer, drawn);
    }
    Some(settled)
}

/// Draws the details of a Message over the top or bottom of `area`: who wrote it and when, who reacted, and where
/// it leads, the link `Enter` opens highlighted.
fn draw_details(
    frame: &mut Frame<'_>,
    area: Rect,
    at_top: bool,
    app: &App,
    details: &Details,
    zone: &TimeZone,
    drawn: &mut Drawn,
) {
    let Some((workspace, message)) = app.message(&details.place, details.ts) else {
        return;
    };
    let width = area.width.min(DETAILS_WIDTH);
    let room = width.saturating_sub(2);
    let mut renderer = Renderer::new(
        app,
        workspace,
        details.place.conversation.workspace,
        room,
        zone,
    );

    let mut header = renderer.header(&message.author);
    if let Author::User(id) = &message.author
        && let Some(user) = workspace.users.get(id)
    {
        for detail in [&user.full_name, &user.title] {
            if !detail.is_empty() && *detail != user.name {
                header.push(Span::styled(format!(" · {detail}"), DIM));
            }
        }
        let theirs = user.time_zone.as_deref();
        if let Some(time) = theirs.and_then(|theirs| their_time(theirs, zone, Timestamp::now())) {
            header.push(Span::styled(format!(" · {time} for them"), DIM));
        }
    }
    let mut posted = zoned(message.ts, zone)
        .map(|z| z.strftime("%a %d %b %Y %H:%M").to_string())
        .unwrap_or_default();
    if message.edited {
        posted.push_str(" · edited");
    }
    let mut lines = vec![
        Line::from(header),
        Line::from(vec![Span::raw("   "), Span::styled(posted, DIM)]),
    ];
    let links = workspace.links(&details.place.conversation.id, message);
    let reactions = &message.reactions;
    if !reactions.is_empty() {
        // Cut the reactions short, rather than push who wrote the Message or where it leads out of the box.
        let around = lines.len() + 2 + links.len();
        let fit = usize::from(area.height.saturating_sub(2)).saturating_sub(around);
        let shown = if reactions.len() > fit {
            fit.saturating_sub(1)
        } else {
            reactions.len()
        };
        lines.push(Line::default());
        lines.extend(
            reactions
                .iter()
                .take(shown)
                .map(|reaction| renderer.reactors(reaction, room)),
        );
        if shown < reactions.len() {
            let more = format!("   … {} more", reactions.len() - shown);
            lines.push(Line::styled(more, DIM));
        }
    }
    lines.push(Line::default());
    let first_link = lines.len();
    for (index, link) in links.iter().enumerate() {
        let number = if index < 9 {
            format!("{} ", index + 1)
        } else {
            "  ".to_owned()
        };
        let icon = match link.kind {
            LinkKind::Web => icons::LINK,
            LinkKind::File => icons::FILE,
            LinkKind::Message => icons::SLACK,
        };
        let mut spans = vec![
            Span::styled(number, DIM),
            Span::styled(icon, ACCENT),
            Span::raw(link.label.clone()),
        ];
        if link.kind == LinkKind::Web && link.label != link.url {
            spans.push(Span::styled(format!("  {}", host(&link.url)), DIM));
        }
        let line = Line::from(spans);
        lines.push(if index == details.selected {
            line.patch_style(SELECTED)
        } else {
            line
        });
    }
    drawn.wanted_pictures.extend(renderer.wanted);
    drawn.next_change = earliest(drawn.next_change, renderer.next_change);

    let rows = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let height = rows.saturating_add(2).min(area.height);
    let popup = Rect {
        y: if at_top {
            area.y
        } else {
            area.bottom() - height
        },
        height,
        width,
        ..area
    };
    let block = Block::bordered().border_style(DIM);
    let inner = block.inner(popup);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    // Keep the selected link in view.
    let skip = (first_link + details.selected + 1).saturating_sub(usize::from(inner.height));
    let shown: Vec<Line<'static>> = lines.into_iter().skip(skip).collect();
    frame.render_widget(Paragraph::new(shown), inner);
}

/// Where a web link goes, in short: its host.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// The time at `now` where someone in the time zone named `theirs` is, unless it's the same as in `ours`.
fn their_time(theirs: &str, ours: &TimeZone, now: Timestamp) -> Option<String> {
    let there = now.to_zoned(TimeZone::get(theirs).ok()?);
    let here = now.to_zoned(ours.clone());
    (there.offset() != here.offset()).then(|| there.strftime("%H:%M").to_string())
}

/// Who reacted: as many of `names` as fit in `room` cells, then how many of the `count` people who reacted that
/// leaves out, as in "Ada, Bob and 3 others".
fn who(names: &[&str], count: usize, room: usize) -> String {
    let others = |named: usize| match (count.saturating_sub(named), named) {
        (0, _) => String::new(),
        (1, 0) => "1 person".to_owned(),
        (left, 0) => format!("{left} people"),
        (1, _) => " and 1 other".to_owned(),
        (left, _) => format!(" and {left} others"),
    };
    let mut listed = String::new();
    let mut named = 0;
    for name in names.iter().take(count) {
        let longer = if named == 0 {
            (*name).to_owned()
        } else {
            format!("{listed}, {name}")
        };
        if longer.width() + others(named + 1).width() > room {
            break;
        }
        listed = longer;
        named += 1;
    }
    listed + &others(named)
}

/// Draws the list of what to complete the word being written with over the bottom left of `area`, just above the
/// draft, the selected one highlighted: whom to @-mention after their icons, emoji after themselves, as `renderer`
/// draws them. The pictures they want, and when they next change, go into `drawn`.
fn draw_suggestions(
    frame: &mut Frame<'_>,
    area: Rect,
    suggestions: &Suggestions,
    mut renderer: Renderer<'_>,
    drawn: &mut Drawn,
) {
    let rows = u16::try_from(suggestions.items.len()).unwrap_or(u16::MAX);
    let height = rows.saturating_add(2).min(area.height);
    let width = area.width.min(SUGGESTIONS_WIDTH);
    let list = Rect {
        y: area.bottom() - height,
        height,
        width,
        ..area
    };
    let block = Block::bordered().border_style(DIM);
    let inner = block.inner(list);
    frame.render_widget(Clear, list);
    frame.render_widget(block, list);
    // Keep the selected one in view.
    let skip = (suggestions.selected + 1).saturating_sub(usize::from(inner.height));
    let lines: Vec<Line<'static>> = suggestions
        .items
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(index, suggestion)| {
            let mut spans = match &suggestion.suggested {
                Suggested::Mention(Mentioned::Person(_)) => {
                    vec![Span::styled(icons::AVATAR, ACCENT)]
                }
                Suggested::Mention(Mentioned::Broadcast(_)) => {
                    vec![Span::styled(icons::BROADCAST, ACCENT)]
                }
                Suggested::Emoji => match renderer.emoji(&Emoji::named(suggestion.name.clone())) {
                    // Followed by a space, in three cells, those of a wide character and a space.
                    Piece::Atom(glyph, cells) => {
                        let pad = usize::from(EMOJI_CELLS.saturating_sub(cells).max(1));
                        vec![glyph, Span::raw(" ".repeat(pad))]
                    }
                    // Its picture isn't loaded yet, or there's nothing to draw it with.
                    Piece::Text(..) => vec![Span::raw(" ".repeat(usize::from(EMOJI_CELLS)))],
                },
            };
            let (name, _) = highlighted(
                &suggestion.name,
                Style::new(),
                &suggestion.positions,
                usize::MAX,
            );
            spans.extend(name);
            if !suggestion.detail.is_empty() {
                spans.push(Span::styled(format!("  {}", suggestion.detail), DIM));
            }
            let line = Line::from(spans);
            if index == suggestions.selected {
                line.patch_style(SELECTED)
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    drawn.wanted_pictures.extend(renderer.wanted);
    drawn.next_change = earliest(drawn.next_change, renderer.next_change);
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
        let mut renderer = Renderer::new(app, workspace, place.conversation.workspace, width, zone);
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
        let mut renderer = Renderer::new(app, workspace, place.conversation.workspace, width, zone);
        let lines = renderer.sending(&outgoing.written.text);
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
/// none, or it's empty and not being written. Its top rule names whom it @-mentions. Returns what's left of `area`
/// above it.
fn draw_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    draft: Option<&Draft>,
    inserting: bool,
) -> Rect {
    let Some(draft) = draft.filter(|draft| inserting || !draft.is_empty()) else {
        return area;
    };
    let lines = u16::try_from(draft.lines().len()).unwrap_or(u16::MAX);
    let height = lines.saturating_add(1).clamp(2, (area.height / 3).max(2));
    let [above, composer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(height)]).areas(area);
    let mentions: Vec<&str> = draft.mentions().collect();
    let mut block = Block::new()
        .borders(Borders::TOP)
        .border_style(if inserting { ACCENT } else { DIM });
    if !mentions.is_empty() {
        block = block.title(Line::styled(format!(" {} ", mentions.join(" · ")), MENTION));
    }
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
    /// Where `workspace` is, to fetch its pictures through.
    key: WorkspaceKey,
    /// Room for content, right of the gutter, in cells.
    width: u16,
    zone: &'a TimeZone,
    /// Pictures it would show, had they been loaded.
    wanted: Vec<WantedPicture>,
    /// When one of its pictures next changes frame, as animation time.
    next_change: Option<Duration>,
    now: Duration,
    /// Whether it lays out a Thread, whose pane counts the Replies itself.
    in_thread: bool,
}

impl<'a> Renderer<'a> {
    /// A renderer for lines `width` cells wide, gutter included, of Messages in `workspace`, which is at `key`.
    fn new(
        app: &'a App,
        workspace: &'a Workspace,
        key: WorkspaceKey,
        width: u16,
        zone: &'a TimeZone,
    ) -> Self {
        Self {
            app,
            workspace,
            key,
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
            content.extend(self.file(file));
        }
        if !message.reactions.is_empty() {
            let mut pieces = Vec::new();
            for reaction in &message.reactions {
                pieces.push(self.emoji(&reaction.emoji));
                let style = if reaction.users.contains(&self.workspace.me) {
                    MENTION
                } else {
                    DIM
                };
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
            Author::User(id) => (
                self.workspace.user_name(id).to_owned(),
                stable_color(id.as_str()),
            ),
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
        let Some(url) = self.workspace.avatar(author) else {
            return fallback;
        };
        match self.app.pictures.state(url) {
            PictureState::Ready(picture) => {
                self.next_change = earliest(self.next_change, picture.next_change(self.now));
                picture.row(0, self.now)
            }
            PictureState::Awaiting { requested } => {
                if !requested {
                    self.want(url, Fit::Emoji);
                }
                fallback
            }
            PictureState::Failed => fallback,
        }
    }

    /// A shared file's lines: an image as [`Renderer::image`] draws it; anything else, or an image that can't be
    /// shown, as its name.
    fn file(&mut self, file: &File) -> Vec<Line<'static>> {
        file.image
            .as_ref()
            .and_then(|image| self.image(image, &file.name))
            .unwrap_or_else(|| vec![labelled(icons::FILE, &file.name)])
    }

    /// An image's lines: its picture, once loaded; until then, a line saying what's coming, by `label`, followed by
    /// as many blank ones as the picture will take. `None` when it can't be shown.
    fn image(&mut self, image: &Image, label: &str) -> Option<Vec<Line<'static>>> {
        match self.app.pictures.state(&image.url) {
            PictureState::Ready(picture) => {
                self.next_change = earliest(self.next_change, picture.next_change(self.now));
                let rows =
                    (0..picture.cells().rows).map(|row| Line::from(picture.row(row, self.now)));
                Some(rows.collect())
            }
            PictureState::Awaiting { requested } => {
                if !requested {
                    self.want(&image.url, Fit::Within(IMAGE));
                }
                let rows = CellBox::fitting(image.width, image.height, IMAGE, self.app.cell).rows;
                let mut lines = vec![labelled(icons::IMAGE, label)];
                lines.resize(usize::from(rows).max(1), Line::default());
                Some(lines)
            }
            PictureState::Failed => None,
        }
    }

    /// Notes that the picture at `url` should be fetched, to be drawn to `fit`.
    fn want(&mut self, url: &str, fit: Fit) {
        self.wanted.push(WantedPicture {
            workspace: self.key,
            url: url.to_owned(),
            fit,
        });
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
                TextBlock::Image { image, alt_text } => {
                    let shown = self.image(image, alt_text);
                    lines.extend(shown.unwrap_or_else(|| vec![labelled(icons::IMAGE, alt_text)]));
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
                    Piece::Text(format!("@{}", self.workspace.user_name(id)), style)
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

    /// A reaction's line in a Message's details, `width` cells wide: its emoji, how many reacted, and who, "you"
    /// for the user.
    fn reactors(&mut self, reaction: &Reaction, width: u16) -> Line<'static> {
        let mut spans = Vec::new();
        let mut used = 0;
        let count = Piece::Text(format!(" {}  ", reaction.count), DIM);
        for piece in [self.emoji(&reaction.emoji), count] {
            let (span, cells) = match piece {
                Piece::Atom(span, cells) => (span, usize::from(cells)),
                Piece::Text(text, style) => {
                    let cells = text.width();
                    (Span::styled(text, style), cells)
                }
            };
            spans.push(span);
            used += cells;
        }
        let me = &self.workspace.me;
        let names: Vec<&str> = reaction
            .users
            .iter()
            .map(|user| {
                if user == me {
                    "you"
                } else {
                    self.workspace.user_name(user)
                }
            })
            .collect();
        let count = usize::try_from(reaction.count).unwrap_or(usize::MAX);
        let room = usize::from(width).saturating_sub(used);
        spans.push(Span::raw(who(&names, count, room)));
        Line::from(spans)
    }

    fn emoji(&mut self, emoji: &Emoji) -> Piece {
        match self.workspace.emojis.drawable(emoji, &self.app.pictures) {
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
                    self.want(url, Fit::Emoji);
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
            let hints = format!(
                "{} of {} · ⏎ open · ^n/^p next/previous · esc cancel",
                search.hits.len(),
                search.searched
            );
            frame.render_widget(Paragraph::new(query), area);
            frame.render_widget(
                Paragraph::new(Line::styled(hints, DIM)).alignment(Alignment::Right),
                area,
            );
            return;
        }
        Mode::Normal | Mode::Insert | Mode::Details(_) => {}
    }
    let mut left = Vec::new();
    if app.mode == Mode::Insert {
        let insert = theme::INSERT;
        left.extend([
            Span::styled(icons::INSERT, insert),
            Span::styled("INSERT  ", insert),
        ]);
    }
    left.extend(connections(app));
    if let Some(notice) = &app.notice {
        left.extend([
            Span::raw("  "),
            Span::styled(icons::ALERT, ALERT),
            Span::styled(notice.clone(), ALERT),
        ]);
    }
    let hints = match (&app.mode, app.focus) {
        (Mode::Details(_), _) => "j/k choose · ⏎ open · 1-9 open · y copy · esc close",
        (Mode::Insert, _) if let Some(suggestions) = &app.suggestions => {
            match suggestions.items[suggestions.selected].suggested {
                Suggested::Mention(_) => "↑/↓ choose · ⇥/⏎ mention · esc normal",
                Suggested::Emoji => "↑/↓ choose · ⇥/⏎ insert · esc normal",
            }
        }
        (Mode::Insert, _) => "⏎ send · ⇧⏎ new line · ^x^e $EDITOR · esc normal",
        (_, Focus::Sidebar) => {
            "j/k select · ⏎ open · za fold · / search · \\ hide · i write · :q quit"
        }
        (_, Focus::Timeline) => {
            "j/k message · ⏎ thread · K details · o open · ^e/^y line · ^d/^u page · g/G · i write · \\ sidebar · :q quit"
        }
        (_, Focus::Thread) => {
            "j/k message · K details · o open · ^e/^y line · i reply · esc close · \\ sidebar · :q quit"
        }
    };
    let left = Line::from(left);
    // The hints give way to what's on the left.
    let room = usize::from(area.width).saturating_sub(left.width() + 2);
    frame.render_widget(Paragraph::new(left), area);
    if hints.width() <= room {
        frame.render_widget(
            Paragraph::new(Line::styled(hints, DIM)).alignment(Alignment::Right),
            area,
        );
    }
}

/// How the Workspaces' connections stand: `live` when every one is, else how each that isn't stands.
fn connections(app: &App) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (_, sign_in) in app.workspaces.iter() {
        let (icon, state, style) = match &sign_in.phase {
            Phase::SigningIn => (icons::LOADING, "signing in".to_owned(), ACCENT),
            Phase::Failed(_) => (icons::ALERT, "signed out".to_owned(), ALERT),
            Phase::Ready(workspace) => match workspace.connection {
                Some(Connection::Live) => continue,
                None => (icons::LOADING, "connecting".to_owned(), ACCENT),
                Some(Connection::Reconnecting { attempt }) => (
                    icons::OFFLINE,
                    format!("reconnecting ({attempt})"),
                    theme::RECONNECTING,
                ),
            },
        };
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.extend([
            Span::styled(icon, style),
            Span::styled(format!("{} {state}", sign_in.label), style),
        ]);
    }
    if spans.is_empty() {
        vec![
            Span::styled(icons::LIVE, theme::LIVE),
            Span::styled("live", theme::LIVE),
        ]
    } else {
        spans
    }
}

/// A dim line: `icon`, then `label`.
fn labelled(icon: &'static str, label: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(icon, DIM),
        Span::styled(label.to_owned(), DIM),
    ])
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

/// A colour for `id`, the same every time: for an author, or a Workspace without an icon.
fn stable_color(id: &str) -> Color {
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
    use slackers_slack::{HistoryPage, Permalinks, Snapshot, Update};

    use super::*;
    use crate::app::Event;
    use crate::config::{ChannelRef, VirtualWorkspace};

    fn w(index: usize) -> WorkspaceKey {
        WorkspaceKey::nth(index)
    }

    const WIDTH: u16 = 80;
    const CELL: slackers_gfx::CellSize = slackers_gfx::CellSize {
        width: 10,
        height: 20,
    };
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
        render_at(app, width, HEIGHT)
    }

    /// Draws `app` `width` cells wide and `height` tall, and returns the screen, what the draw reported, and the
    /// cells' styles.
    fn render_at(
        app: &App,
        width: u16,
        height: u16,
    ) -> (Vec<String>, Drawn, ratatui::buffer::Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut drawn = Drawn::default();
        terminal.draw(|frame| drawn = draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..height)
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

    fn channel(id: &str, name: &str) -> (Conversation, Activity) {
        let channel = Conversation {
            id: ConversationId::new(id),
            kind: ConversationKind::Channel { private: false },
            name: name.into(),
        };
        (channel, Activity::default())
    }

    /// A Workspace called `name`, holding `conversations`.
    fn ready(
        name: &str,
        icon: Option<&str>,
        conversations: Vec<(Conversation, Activity)>,
    ) -> Update {
        Update::Ready(Box::new(Snapshot {
            workspace: WorkspaceId::new("T1"),
            name: name.into(),
            icon: icon.map(str::to_owned),
            me: UserId::new("UME"),
            permalinks: Permalinks::new("https://test.slack.com/"),
            conversations,
            followed_threads: Vec::new(),
        }))
    }

    /// An app with `#general` open, holding one Message.
    fn reading_general() -> App {
        let mut app = App::new(vec!["test".into()], Vec::new(), CELL);
        let snapshot = ready("Test", None, vec![channel("C1", "general")]);
        app.update(Event::Slack(w(0), snapshot));
        type_text(&mut app, "/general");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let page = HistoryPage {
            messages: vec![reading_general_message()],
            has_more: false,
        };
        app.update(Event::Slack(
            w(0),
            Update::History {
                conversation: ConversationId::new("C1"),
                result: Ok(page),
            },
        ));
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
    fn whom_to_mention_shows_over_the_messages_and_the_draft_names_whom_it_mentions() {
        let mut app = reading_general();
        type_text(&mut app, "i@");
        let (rows, _, buffer) = render_sized(&app, WIDTH);
        let row_of = |text: &str| rows.iter().position(|row| row.contains(text)).unwrap();
        let (here, channel, draft) = (row_of("here"), row_of("channel"), row_of("@"));
        assert!(here < channel && channel + 2 < draft, "{rows:#?}");
        let column = rows[here].find("here").unwrap();
        let x = u16::try_from(rows[here][..column].chars().count()).unwrap();
        let y = u16::try_from(here).unwrap();
        assert_eq!(
            buffer[(x, y)].bg,
            SELECTED.bg.unwrap(),
            "the first is selected"
        );
        assert!(rows.last().unwrap().contains("⇥/⏎ mention"), "{rows:#?}");

        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        let rows = screen(&app);
        let draft = rows.iter().position(|row| row.contains("│ @here")).unwrap();
        assert!(!rows.iter().any(|row| row.contains("channel")), "{rows:#?}");
        assert!(
            rows[draft - 1].contains(" @here ─"),
            "the rule names it: {rows:#?}"
        );
    }

    #[test]
    fn emoji_to_insert_show_as_themselves_by_name_and_custom_ones_load() {
        let mut app = reading_general();
        let mut custom = slackers_model::CustomEmojiSet::default();
        custom.insert_image("parrot", "https://e/parrot.gif");
        let list = slackers_slack::EmojiList {
            custom,
            standard: vec!["+1".into(), "partying_face".into()],
        };
        app.update(Event::Slack(w(0), Update::Emoji(list)));
        type_text(&mut app, "i:pa");
        let (rows, drawn) = render(&app);
        let row_of = |text: &str| rows.iter().position(|row| row.contains(text)).unwrap();
        let (partying, parrot) = (row_of("partying_face"), row_of("parrot"));
        assert_eq!(partying + 1, parrot, "{rows:#?}");
        assert!(rows[partying].contains("🥳"), "{rows:#?}");
        assert!(rows.last().unwrap().contains("⇥/⏎ insert"), "{rows:#?}");
        let wanted: Vec<(&str, Fit)> = drawn
            .wanted_pictures
            .iter()
            .map(|wanted| (wanted.url.as_str(), wanted.fit))
            .collect();
        assert_eq!(wanted, [("https://e/parrot.gif", Fit::Emoji)]);

        for _ in 0..2 {
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        }
        type_text(&mut app, "thumbsu");
        let rows = screen(&app);
        let found = rows.iter().find(|row| row.contains("👍"));
        assert!(
            found.is_some_and(|row| row.contains("+1  thumbsup")),
            "{rows:#?}"
        );
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

        assert!(rows[0].starts_with(&format!("T {}general", icons::CHANNEL)));

        type_text(&mut app, "x");
        let rows = screen(&app);
        assert!(!rows.iter().any(|row| row.starts_with("T ")), "{rows:#?}");
        assert!(rows.last().unwrap().contains("0 of 1"));
    }

    #[test]
    fn messages_hang_off_their_authors_avatar() {
        let mut app = reading_general();
        let ana = slackers_slack::User {
            id: UserId::new("U2"),
            name: "Ana".into(),
            full_name: String::new(),
            title: String::new(),
            time_zone: None,
            avatar: Some("https://a/ana.png".into()),
        };
        app.update(Event::Slack(w(0), Update::User(ana)));
        let follow_up = Message {
            ts: Ts::from_micros(1_700_000_060_000_000),
            content: RichText::plain("a minute later"),
            ..reading_general_message()
        };
        let posted = slackers_slack::LiveEvent::Posted {
            conversation: ConversationId::new("C1"),
            message: follow_up,
        };
        app.update(Event::Slack(w(0), Update::Live(posted)));

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
        let ana = WantedPicture {
            workspace: w(0),
            url: "https://a/ana.png".into(),
            fit: Fit::Emoji,
        };
        assert_eq!(drawn.wanted_pictures, [ana]);
    }

    #[test]
    fn an_image_takes_its_room_before_it_loads_and_its_name_if_it_cant() {
        let mut app = reading_general();
        let shot = File {
            name: "shot.png".into(),
            mimetype: "image/png".into(),
            link: None,
            image: Some(Image {
                url: "https://f/shot.png".into(),
                width: 300,
                height: 200,
            }),
        };
        let start = reading_general_message().ts.as_micros();
        let later = |seconds: u64, text: &str, files| Message {
            ts: Ts::from_micros(start + seconds * 1_000_000),
            content: RichText::plain(text),
            files,
            ..reading_general_message()
        };
        for message in [
            later(60, "look", vec![shot]),
            later(120, "after", Vec::new()),
        ] {
            let posted = slackers_slack::LiveEvent::Posted {
                conversation: ConversationId::new("C1"),
                message,
            };
            app.update(Event::Slack(w(0), Update::Live(posted)));
        }

        let (rows, drawn, _) = render_at(&app, WIDTH, 24);
        let row =
            |rows: &[String], text: &str| rows.iter().position(|row| row.contains(text)).unwrap();
        // 300 × 200 pixels on 10 × 20 pixel cells: 30 columns by 10 rows, kept free while it loads.
        assert_eq!(
            row(&rows, "after") - row(&rows, "shot.png"),
            10,
            "{rows:#?}"
        );
        assert!(rows[row(&rows, "shot.png")].contains(&format!("{}shot.png", icons::IMAGE)));
        let wanted = WantedPicture {
            workspace: w(0),
            url: "https://f/shot.png".into(),
            fit: Fit::Within(IMAGE),
        };
        assert!(
            drawn.wanted_pictures.contains(&wanted),
            "{:?}",
            drawn.wanted_pictures
        );

        app.update(Event::PictureFailed {
            url: "https://f/shot.png".into(),
        });
        let (rows, _, _) = render_at(&app, WIDTH, 24);
        assert_eq!(row(&rows, "after") - row(&rows, "shot.png"), 1);
        assert!(rows[row(&rows, "shot.png")].contains(&format!("{}shot.png", icons::FILE)));
    }

    #[test]
    fn an_image_block_shows_like_an_image_file() {
        let mut app = reading_general();
        let gif = slackers_model::Block::Image {
            image: Image {
                url: "https://media4.giphy.com/x.gif".into(),
                width: 200,
                height: 100,
            },
            alt_text: "a dancing cat".into(),
        };
        let start = reading_general_message().ts.as_micros();
        let posted = slackers_slack::LiveEvent::Posted {
            conversation: ConversationId::new("C1"),
            message: Message {
                ts: Ts::from_micros(start + 60_000_000),
                author: Author::Bot {
                    name: "giphy".into(),
                    avatar: None,
                },
                content: RichText { blocks: vec![gif] },
                ..reading_general_message()
            },
        };
        app.update(Event::Slack(w(0), Update::Live(posted)));

        let (rows, drawn, _) = render_at(&app, WIDTH, 24);
        let label = rows
            .iter()
            .position(|row| row.contains("a dancing cat"))
            .unwrap();
        assert!(rows[label].contains(&format!("{}a dancing cat", icons::IMAGE)));
        // 200 × 100 pixels on 10 × 20 pixel cells: 5 rows, the last of the pane's above the status line.
        assert_eq!(label + 5, rows.len() - 1, "{rows:#?}");
        let wanted = WantedPicture {
            workspace: w(0),
            url: "https://media4.giphy.com/x.gif".into(),
            fit: Fit::Within(IMAGE),
        };
        assert!(drawn.wanted_pictures.contains(&wanted));

        app.update(Event::PictureFailed {
            url: "https://media4.giphy.com/x.gif".into(),
        });
        let rows = render_at(&app, WIDTH, 24).0;
        assert_eq!(
            rows.iter().position(|row| row.contains("a dancing cat")),
            Some(rows.len() - 2),
            "its words alone, once it can't be shown"
        );
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
        let replies = Update::Replies {
            conversation: ConversationId::new("C1"),
            parent: parent.ts,
            result: Ok(vec![parent, reply]),
        };
        app.update(Event::Slack(w(0), replies));
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
        let rows = screen(&app);
        assert!(rows[0].starts_with(&format!("{}{}Threads", icons::UNFOLDED, icons::THREAD)));
        assert!(
            rows[1].starts_with(&format!("{}T Test", icons::UNFOLDED)),
            "then the Workspace's section"
        );

        type_text(&mut app, "\\");
        let rows = screen(&app);
        // The Workspace's initial stands in for its icon.
        let breadcrumb = format!(" T Test {}{}general", icons::CRUMB, icons::CHANNEL);
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
        let breadcrumb = format!(" T Test {}{}thread", icons::CRUMB, icons::THREAD);
        assert!(rows[0].starts_with(&breadcrumb), "{:?}", rows[0]);

        let (rows, _, _) = render_sized(&app, 200);
        let only_leftmost = rows[0].contains("thread in") && rows[0].matches("Test").count() == 1;
        assert!(only_leftmost, "{:?}", rows[0]);
    }

    /// An app signed in to `test` and `other`, the Virtual Workspace `K` holding `test#random`, unread, and
    /// `other#dev`, with a Mention. `other` has an icon, not loaded yet.
    fn two_workspaces() -> App {
        let channels = |text: &[&str]| {
            text.iter()
                .map(|text| {
                    let (workspace, channel) = text.split_once('#').unwrap();
                    ChannelRef {
                        workspace: workspace.into(),
                        channel: channel.into(),
                    }
                })
                .collect()
        };
        let virtual_workspaces = vec![VirtualWorkspace {
            name: "K".into(),
            channels: channels(&["test#random", "other#dev"]),
        }];
        let mut app = App::new(
            vec!["test".into(), "other".into()],
            virtual_workspaces,
            CELL,
        );
        let unread = Activity::counted(Ts::from_micros(1), Ts::from_micros(5), true, 0);
        let random = (channel("C2", "random").0, unread);
        let test = ready("Test", None, vec![channel("C1", "general"), random]);
        app.update(Event::Slack(w(0), test));
        let mentioned = Activity::counted(Ts::from_micros(1), Ts::from_micros(5), true, 2);
        let dev = (channel("C6", "dev").0, mentioned);
        let other = ready("Other", Some("https://i/other.png"), vec![dev]);
        app.update(Event::Slack(w(1), other));
        app
    }

    #[test]
    fn virtual_workspaces_mark_each_channel_with_its_workspace() {
        let app = two_workspaces();
        let (rows, drawn, _) = render_sized(&app, WIDTH);
        let sidebar: Vec<String> = rows
            .iter()
            .map(|row| {
                row.chars()
                    .take(29)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        // Below the Threads section, empty here.
        let sidebar = &sidebar[1..];
        let header = format!("{}{}K", icons::UNFOLDED, icons::VIRTUAL_WORKSPACE);
        assert!(sidebar[0].starts_with(&header), "{sidebar:#?}");
        assert!(
            sidebar[0].ends_with(&format!("{}2", icons::MENTION)),
            "its Channels' Mentions add up: {sidebar:#?}"
        );
        assert!(sidebar[1].starts_with(&format!("T {}random", icons::CHANNEL)));
        assert!(sidebar[1].ends_with(icons::UNREAD.trim_end()));
        assert!(sidebar[2].starts_with(&format!("O {}dev", icons::CHANNEL)));
        assert!(sidebar[3].starts_with(&format!("{}T Test", icons::UNFOLDED)));
        assert!(
            sidebar[4].starts_with(&format!("  {}general", icons::CHANNEL)),
            "a Workspace's own section needs no mark: {sidebar:#?}"
        );
        assert!(sidebar[5].starts_with(&format!("{}O Other", icons::UNFOLDED)));
        let icon = WantedPicture {
            workspace: w(1),
            url: "https://i/other.png".into(),
            fit: Fit::Emoji,
        };
        assert!(drawn.wanted_pictures.contains(&icon));
    }

    #[test]
    fn the_status_line_says_which_workspaces_arent_live() {
        let mut app = App::new(vec!["test".into(), "other".into()], Vec::new(), CELL);
        app.update(Event::Slack(w(0), ready("Test", None, Vec::new())));
        app.update(Event::Slack(w(0), Update::Connection(Connection::Live)));
        let status = screen(&app).pop().unwrap();
        assert!(status.contains("other signing in"), "{status}");
        assert!(!status.contains("test"), "{status}");

        app.update(Event::CantSignIn {
            workspace: w(1),
            reason: "no token".into(),
        });
        let rows = screen(&app);
        assert!(rows.last().unwrap().contains("other signed out"));
        assert!(
            rows.iter()
                .any(|row| row.contains("couldn't sign in to other"))
        );

        let mut app = App::new(vec!["test".into()], Vec::new(), CELL);
        app.update(Event::Slack(w(0), ready("Test", None, Vec::new())));
        app.update(Event::Slack(w(0), Update::Connection(Connection::Live)));
        let status = screen(&app).pop().unwrap();
        assert!(
            status.starts_with(&format!("{}live", icons::LIVE)),
            "{status}"
        );
    }

    #[test]
    fn a_hidden_sidebar_names_the_virtual_workspace_and_the_channels_workspace() {
        let mut app = two_workspaces();
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let rows = screen(&app);
        assert!(
            rows[0][rows[0].find(' ').unwrap()..].contains(&format!(" O {}dev", icons::CHANNEL)),
            "{:?}",
            rows[0]
        );
        press(&mut app, KeyCode::Char('\\'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('\\'), KeyModifiers::NONE);
        let breadcrumb = format!(
            " {}K {}O {}dev",
            icons::VIRTUAL_WORKSPACE,
            icons::CRUMB,
            icons::CHANNEL
        );
        assert!(
            screen(&app)[0].starts_with(&breadcrumb),
            "{:?}",
            screen(&app)[0]
        );
    }

    #[test]
    fn a_followed_thread_with_replies_to_read_shows_its_gist_and_badge() {
        let mut app = App::new(vec!["test".into()], Vec::new(), CELL);
        let parent = Message {
            content: RichText::plain("can   someone\nreview this?"),
            ..reading_general_message()
        };
        let read = parent.ts;
        let thread = slackers_slack::FollowedThread {
            conversation: ConversationId::new("C1"),
            activity: Activity::counted(read, Ts::from_micros(read.as_micros() + 1), true, 0),
            parent,
        };
        let snapshot = Snapshot {
            workspace: WorkspaceId::new("T1"),
            name: "Test".into(),
            icon: None,
            me: UserId::new("UME"),
            permalinks: Permalinks::new("https://test.slack.com/"),
            conversations: vec![channel("C1", "general")],
            followed_threads: vec![thread],
        };
        app.update(Event::Slack(w(0), Update::Ready(Box::new(snapshot))));
        let rows = screen(&app);
        let row: String = rows[1].chars().take(29).collect();
        // Its words run together on one line, cut to fit.
        assert!(
            row.starts_with(&format!("T {}general: can someone …", icons::THREAD)),
            "{row:?}"
        );
        assert!(
            row.trim_end().ends_with(icons::UNREAD.trim_end()),
            "{row:?}"
        );
        let header: String = rows[0].chars().take(29).collect();
        assert!(
            header.trim_end().ends_with(icons::UNREAD.trim_end()),
            "the header adds it up: {header:?}"
        );
    }

    #[test]
    fn command_mode_takes_the_status_line() {
        let mut app = reading_general();
        type_text(&mut app, ":q");
        assert!(screen(&app).last().unwrap().starts_with(":q"));
    }

    #[test]
    fn who_reacted_names_as_many_as_fit_and_counts_the_rest() {
        let names = ["Ada", "Bob", "you"];
        assert_eq!(who(&names, 3, 40), "Ada, Bob, you");
        assert_eq!(who(&names, 5, 40), "Ada, Bob, you and 2 others");
        assert_eq!(who(&names, 4, 40), "Ada, Bob, you and 1 other");
        assert_eq!(
            who(&names, 5, "Ada, Bob and 3 others".len()),
            "Ada, Bob and 3 others"
        );
        assert_eq!(who(&names, 5, 8), "5 people", "not even one name fits");
        assert_eq!(who(&[], 1, 8), "1 person");
    }

    #[test]
    fn someone_elses_time_shows_only_when_it_differs_from_ours() {
        let midnight_utc: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            their_time("Asia/Tokyo", &TimeZone::UTC, midnight_utc),
            Some("09:00".into())
        );
        assert_eq!(their_time("Etc/UTC", &TimeZone::UTC, midnight_utc), None);
        assert_eq!(
            their_time("Nowhere/Land", &TimeZone::UTC, midnight_utc),
            None
        );
    }

    #[test]
    fn many_reactions_give_way_to_who_wrote_the_message_and_where_it_leads() {
        let mut app = reading_general();
        let reactions = [
            "wave", "tada", "eyes", "rocket", "fire", "heart", "100", "pray",
        ]
        .into_iter()
        .map(|name| Reaction {
            emoji: Emoji::named(name),
            users: vec![UserId::new("U4")],
            count: 1,
        })
        .collect();
        let message = Message {
            reactions,
            ..reading_general_message()
        };
        let edited = slackers_slack::LiveEvent::Edited {
            conversation: ConversationId::new("C1"),
            message,
        };
        app.update(Event::Slack(w(0), Update::Live(edited)));

        type_text(&mut app, "K");
        let (rows, _, _) = render_at(&app, WIDTH, 13);
        let found = |text: &str| rows.iter().any(|row| row.contains(text));
        assert!(found("Tue 14 Nov 2023"), "{rows:#?}");
        assert!(found("… 5 more"), "{rows:#?}");
        assert!(found("this message in Slack"), "{rows:#?}");
    }

    #[test]
    fn a_messages_details_name_who_reacted_and_where_it_leads() {
        let mut app = reading_general();
        for (id, name, full_name) in [("U2", "Ana", "Ana Lima"), ("U4", "Bob", "")] {
            let user = slackers_slack::User {
                id: UserId::new(id),
                name: name.into(),
                full_name: full_name.into(),
                title: "Engineer".into(),
                time_zone: None,
                avatar: None,
            };
            app.update(Event::Slack(w(0), Update::User(user)));
        }
        let message = Message {
            content: RichText {
                blocks: vec![TextBlock::Paragraph(vec![Inline::Link {
                    url: "https://docs.rs/ratatui/latest".into(),
                    label: Some("the docs".into()),
                }])],
            },
            reactions: vec![Reaction {
                emoji: Emoji::named("wave"),
                users: vec![UserId::new("U4"), UserId::new("UME")],
                count: 2,
            }],
            edited: true,
            ..reading_general_message()
        };
        let edited = slackers_slack::LiveEvent::Edited {
            conversation: ConversationId::new("C1"),
            message,
        };
        app.update(Event::Slack(w(0), Update::Live(edited)));

        type_text(&mut app, "K");
        let (rows, _, buffer) = render_sized(&app, WIDTH);
        let row_of = |text: &str| {
            rows.iter()
                .position(|row| row.contains(text))
                .unwrap_or_else(|| panic!("no {text:?} in {rows:#?}"))
        };
        let author = row_of("Ana · Ana Lima · Engineer");
        assert!(rows[author + 1].contains("· edited"), "{rows:#?}");
        let reactors = row_of("2  Bob, you");
        let docs = row_of("1 ");
        assert!(rows[docs].contains("the docs  docs.rs"), "{rows:#?}");
        assert!(author < reactors && reactors < docs, "{rows:#?}");
        assert!(rows[docs + 1].contains("2 "), "{rows:#?}");
        assert!(
            rows[docs + 1].contains("this message in Slack"),
            "{rows:#?}"
        );
        let column = rows[docs].find("the docs").unwrap();
        let x = u16::try_from(rows[docs][..column].chars().count()).unwrap();
        let y = u16::try_from(docs).unwrap();
        assert_eq!(
            buffer[(x, y)].bg,
            SELECTED.bg.unwrap(),
            "the first link is selected"
        );
        assert!(rows.last().unwrap().contains("esc close"), "{rows:#?}");

        type_text(&mut app, "q");
        assert!(
            !screen(&app)
                .iter()
                .any(|row| row.contains("this message in Slack"))
        );
    }
}
