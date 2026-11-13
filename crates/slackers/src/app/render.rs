//! Drawing what slackers knows into the kit's buffers: the sidebar, the sign-in page, Activity, and the Messages of
//! a Conversation or a Thread, each a block of lines in highlight groups (the kit's D3, D7). Spans carry what they
//! refer to ([`Target`]), and blocks the Message, Activity item or section they show.

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use ratatui::style::{Modifier, Style};
use slackers_model::{
    Author, Block as TextBlock, Button, Conversation, ConversationKind, Emoji, File, Happened,
    Image, Inline, MentionOf, Message, Presence, Reaction, RichText, Style as TextStyle, Ts,
    Unread,
};
use slackers_slack::Request;
use unicode_width::UnicodeWidthStr as _;
use wcgw_kit::BufferId;
use wcgw_kit::doc::{BlockKey, Content, Document, Fold};
use wcgw_kit::hl::Group;

use super::navigate::{NEW, when};
use super::workspace::{Choice, LinkKind, broadcast_name, is_web};
use super::{
    ConversationRef, Effects, Kit, Phase, Place, SIDEBAR_WIDTH, Slackers, Target, Workspace,
    WorkspaceKey, slack,
};
use crate::activity::{Entry, Fetched};
use crate::emoji::Drawable;
use crate::hl::{
    ACCENT, ACTIVE, ALERT, BOT, BUTTON, CODE, DIM, HEADER, LINK, MENTION, MENTION_ME, NEW as RULE,
    OPEN, PRIMARY_BUTTON, QUOTE, READ, UNREAD,
};
use crate::icons;
use crate::layout::{self, Piece as LayoutPiece};
use crate::sidebar::{Item, Section};

type Span = wcgw_kit::doc::Span<Target>;
type Line = wcgw_kit::doc::Line<Target>;
type Piece = LayoutPiece<Target>;

/// Cells left of a Message's content, holding its author's avatar: two cells of picture and a space.
const GUTTER: u16 = 3;

/// The most room an image shared in a Message takes, in cells; one smaller than this shows at its own size.
const IMAGE: (u16, u16) = (60, 15);

/// Messages by the same author less than this far apart share one header.
const GROUP_WITHIN: u64 = 5 * 60 * 1_000_000;

/// Cells left of a sidebar row's name: a Workspace's icon or a fold marker, then an icon for what the row is.
const LEAD: usize = 4;

/// The room left of an Activity item's words: whether it's unread, its Workspace's icon, and what happened.
const ACTIVITY_LEAD: usize = 7;

/// The room an emoji gets before words, a space after it included.
const EMOJI_CELLS: usize = 3;

/// The most people named per reaction in a Message's details, whose names are looked up: more wouldn't fit on the
/// reaction's line.
const NAMED_REACTORS: usize = 12;

/// The widest a Message's details are, as the kit draws its hover card.
const DETAILS_WIDTH: u16 = 58;

/// Where the picture at `url` is, as the kit asks slackers for it: the Workspace to fetch it through (Slack serves a
/// Workspace's files to its members only), and the URL.
pub(crate) fn picture_source(key: WorkspaceKey, url: &str) -> String {
    format!("{} {url}", key.index())
}

/// The Workspace and the URL `source`, made by [`picture_source`], stands for.
pub(crate) fn picture_of(source: &str) -> Option<(WorkspaceKey, &str)> {
    let (index, url) = source.split_once(' ')?;
    Some((WorkspaceKey::nth(index.parse().ok()?), url))
}

impl Slackers {
    /// Draws the sidebar: Activity's row, then each section under a header that folds it.
    pub(super) fn render_sidebar(&mut self, kit: &mut Kit) {
        let shown = self.shown_places(kit);
        let activity_shown = self.activity_buffer.is_some_and(|buffer| {
            kit.window_ids()
                .into_iter()
                .any(|window| kit.window_buffer(window) == Some(buffer))
        });
        // The rows of what windows show, in their group's background: the cursor's row is `CursorLine`'s.
        let tint = kit.style(OPEN).bg.map(|bg| Style::new().bg(bg));
        let open = |spans: Vec<Span>, is_open: bool| match (tint, is_open) {
            (Some(tint), true) => spans
                .into_iter()
                .map(|span| span.with_style(tint))
                .collect(),
            _ => spans,
        };
        let width = usize::from(
            self.sidebar_window(kit)
                .and_then(|window| kit.window_text_area(window))
                .map_or(SIDEBAR_WIDTH, |area| area.width),
        );
        let mut blocks: Vec<(BlockKey, Content<Target>)> = Vec::new();
        // Each row's line, counting from Activity's, for the first with something unread.
        let mut line = 1;
        let mut first_unread = None;
        let activity = open(self.activity_row(kit, width), activity_shown);
        blocks.push((
            BlockKey::new("activity"),
            Content {
                lines: vec![Line::Text(activity)],
                folds: Vec::new(),
                target: Some(Target::Activity),
            },
        ));
        let mut keys = std::collections::HashSet::new();
        for (section, items) in self.sidebar.sections() {
            // Two Virtual Workspaces of one name, as a config edited by hand may have, are two sections.
            let mut key = section.key();
            while !keys.insert(key.clone()) {
                key.push('\'');
            }
            let badge = items
                .iter()
                .map(|item| self.badge_of_item(item))
                .fold(Badge::default(), Badge::plus);
            let header = |folded| {
                let row = self.header_row(kit, section, folded, badge, width);
                Line::Text(
                    row.into_iter()
                        .map(|span| span.with_target(Target::Fold))
                        .collect(),
                )
            };
            let mut lines = vec![header(false)];
            line += 1;
            for item in items {
                if first_unread.is_none() && self.badge_of_item(item).unread {
                    first_unread = Some(line);
                }
                line += 1;
                let (row, is_open) = match item {
                    Item::Conversation(conversation) => {
                        // Only a Workspace's own section needs no mark of the Workspace on each row.
                        let marked = !matches!(section, Section::Workspace(_));
                        let is_open = shown.contains(&Place::of(conversation));
                        (
                            self.conversation_row(kit, conversation, marked, width),
                            is_open,
                        )
                    }
                    Item::Thread {
                        conversation,
                        parent,
                    } => {
                        let place = Place {
                            conversation: conversation.clone(),
                            thread: Some(*parent),
                        };
                        let is_open = shown.contains(&place);
                        (self.thread_row(kit, conversation, *parent, width), is_open)
                    }
                };
                let target = match item {
                    Item::Conversation(conversation) => Target::Conversation(conversation.clone()),
                    Item::Thread {
                        conversation,
                        parent,
                    } => Target::Thread {
                        conversation: conversation.clone(),
                        parent: *parent,
                    },
                };
                let row = row
                    .into_iter()
                    .map(|span| span.with_target(target.clone()))
                    .collect();
                lines.push(Line::Text(open(row, is_open)));
            }
            let folded = lines.len();
            blocks.push((
                BlockKey::new(key),
                Content {
                    lines,
                    folds: vec![Fold {
                        lines: 0..folded,
                        summary: header(true),
                        closed: false,
                    }],
                    target: None,
                },
            ));
        }
        self.first_unread = first_unread;
        kit.buffer_mut(self.sidebar_buffer)
            .doc_mut()
            .set_blocks(blocks);
    }

    /// Activity's row: its icon and name, bold while items are unread, and on the right how many are.
    fn activity_row(&self, kit: &Kit, width: usize) -> Vec<Span> {
        let unread = self.activity.unread();
        let (icon_group, name_group, badge) = if unread > 0 {
            (
                ACCENT,
                UNREAD,
                vec![Span::new(unread.to_string(), MENTION_ME)],
            )
        } else {
            (DIM, READ, Vec::new())
        };
        let lead = vec![
            Span::new("  ", DIM),
            Span::new(kit.icon(icons::ACTIVITY), icon_group),
        ];
        entry(lead, "Activity", name_group, badge, width)
    }

    /// A section's header: whether it's folded, its icon and name, and the combined badge of what it holds.
    fn header_row(
        &self,
        kit: &Kit,
        section: &Section,
        folded: bool,
        badge: Badge,
        width: usize,
    ) -> Vec<Span> {
        let fold = kit.icon(if folded {
            icons::FOLDED
        } else {
            icons::UNFOLDED
        });
        let (icon, name) = self.section_title(kit, section);
        entry(
            vec![Span::new(fold, DIM), icon],
            &name,
            HEADER,
            badge.spans(kit),
            width,
        )
    }

    /// A section's icon and name: the Threads', a Virtual Workspace's, or a Workspace's, whose icon says how signing
    /// in to it went.
    pub(super) fn section_title(&self, kit: &Kit, section: &Section) -> (Span, String) {
        match section {
            Section::Threads => (
                Span::new(kit.icon(icons::THREAD), ACCENT),
                "Threads".to_owned(),
            ),
            Section::VirtualWorkspace(name) => (
                Span::new(kit.icon(icons::VIRTUAL_WORKSPACE), ACCENT),
                name.clone(),
            ),
            Section::Workspace(key) => {
                let Some(sign_in) = self.workspaces.sign_in(*key) else {
                    return (Span::new("  ", DIM), String::new());
                };
                match &sign_in.phase {
                    Phase::SigningIn => (
                        Span::new(kit.icon(icons::LOADING), ACCENT),
                        sign_in.label.clone(),
                    ),
                    Phase::Failed(_) => (
                        Span::new(kit.icon(icons::ALERT), ALERT),
                        sign_in.label.clone(),
                    ),
                    Phase::Ready(workspace) => {
                        (workspace_marker(*key, workspace), workspace.name.clone())
                    }
                }
            }
        }
    }

    /// A Conversation's row: when `marked`, its Workspace's icon; an icon for its kind; its name, bold when unread;
    /// and on the right its badge.
    fn conversation_row(
        &self,
        kit: &Kit,
        conversation: &ConversationRef,
        marked: bool,
        width: usize,
    ) -> Vec<Span> {
        let Some((workspace, found)) = self.workspaces.conversation(conversation) else {
            return Vec::new();
        };
        let badge = Badge::of(workspace.unread.get(&conversation.id));
        let (icon, name) = label(workspace, found);
        let marker = if marked {
            workspace_marker(conversation.workspace, workspace)
        } else {
            Span::new("  ", DIM)
        };
        let (icon_group, name_group) = if badge.unread {
            (ACCENT, UNREAD)
        } else {
            (DIM, READ)
        };
        let active = match &found.kind {
            ConversationKind::Dm { with } => workspace.presence(with) == Some(Presence::Active),
            ConversationKind::Channel { .. } | ConversationKind::GroupDm => false,
        };
        let icon = if active {
            Span::new(kit.icon(icons::DM_ACTIVE), ACTIVE)
        } else {
            Span::new(kit.icon(icon), icon_group)
        };
        entry(
            vec![marker, icon],
            name,
            name_group,
            badge.spans(kit),
            width,
        )
    }

    /// A Followed Thread's row: its Workspace's icon; a Thread icon; its Conversation's name and the gist of its parent
    /// (bold when there are Replies to read); and on the right its badge.
    fn thread_row(
        &self,
        kit: &Kit,
        conversation: &ConversationRef,
        parent: Ts,
        width: usize,
    ) -> Vec<Span> {
        let Some((workspace, found)) = self.workspaces.conversation(conversation) else {
            return Vec::new();
        };
        let followed = workspace.followed.get(&(conversation.id.clone(), parent));
        let badge = Badge::of(followed.map(|followed| &followed.unread));
        let (_, name) = label(workspace, found);
        let parent_message = followed
            .and_then(|followed| followed.parent.as_ref())
            .or_else(|| workspace.timelines.get(&conversation.id)?.get(parent));
        let text = match parent_message.map(|message| workspace.gist(&message.content)) {
            Some(gist) if !gist.is_empty() => format!("{name}: {gist}"),
            _ => name.to_owned(),
        };
        let (icon_group, name_group) = if badge.unread {
            (ACCENT, UNREAD)
        } else {
            (DIM, READ)
        };
        let lead = vec![
            workspace_marker(conversation.workspace, workspace),
            Span::new(kit.icon(icons::THREAD), icon_group),
        ];
        entry(lead, &text, name_group, badge.spans(kit), width)
    }

    /// What `item`'s row says about unread Messages: in a Conversation, or in a Followed Thread.
    fn badge_of_item(&self, item: &Item) -> Badge {
        match item {
            Item::Conversation(conversation) => Badge::of(self.workspaces.unread(conversation)),
            Item::Thread {
                conversation,
                parent,
            } => Badge::of(
                self.workspaces
                    .followed(conversation, *parent)
                    .map(|followed| &followed.unread),
            ),
        }
    }

    /// Draws how signing in goes for each Workspace.
    pub(super) fn render_sign_in(&mut self, kit: &mut Kit) {
        let mut lines = vec![Line::text("")];
        for (_, sign_in) in self.workspaces.iter() {
            let (icon, group, said) = match &sign_in.phase {
                Phase::SigningIn => (
                    icons::LOADING,
                    ACCENT,
                    format!("signing in to {}…", sign_in.label),
                ),
                Phase::Ready(workspace) => (
                    icons::SIGNED_IN,
                    ACTIVE,
                    format!("signed in to {}", workspace.name),
                ),
                Phase::Failed(_) => (
                    icons::ALERT,
                    ALERT,
                    format!("couldn't sign in to {}", sign_in.label),
                ),
            };
            lines.push(Line::Text(vec![
                Span::new("  ", Group::NORMAL),
                Span::new(kit.icon(icon), group),
                Span::new(said, Group::NORMAL),
            ]));
            if let Phase::Failed(reason) = &sign_in.phase {
                lines.push(Line::Text(vec![Span::new(format!("    {reason}"), DIM)]));
            }
        }
        let doc = kit.buffer_mut(self.sign_in_buffer).doc_mut();
        doc.set_blocks([(BlockKey::new("sign in"), Content::from(lines))]);
    }

    /// Draws Activity in `buffer`: every item, newest first, two lines each; and asks for the Messages they're about
    /// that aren't loaded.
    pub(super) fn render_activity(&mut self, kit: &mut Kit, buffer: BufferId) -> Effects {
        let mut effects = Vec::new();
        let wanted: Vec<(WorkspaceKey, slackers_model::MessageRef)> = self
            .activity
            .entries()
            .iter()
            .filter(|entry| {
                self.activity_message(entry.workspace, &entry.item.message) == Fetched::Unasked
            })
            .map(|entry| (entry.workspace, entry.item.message.clone()))
            .collect();
        for (key, at) in wanted {
            if let Some(request) = self.activity.fetch(key, &at) {
                effects.push(slack(key, request));
            }
        }
        let width = usize::from(kit.text_width(buffer));
        self.widths.insert(buffer, kit.text_width(buffer));
        let now = Timestamp::now();
        let entries = self.activity.entries();
        let mut blocks: Vec<(BlockKey, Content<Target>)> = entries
            .iter()
            .map(|entry| {
                let key = format!("{} {}", entry.workspace.index(), entry.item.key);
                let lines = self.activity_item(kit, entry, width, now);
                (
                    BlockKey::new(key),
                    Content {
                        lines: lines.into(),
                        folds: Vec::new(),
                        target: Some(Target::ActivityItem(
                            entry.workspace,
                            entry.item.key.clone(),
                        )),
                    },
                )
            })
            .collect();
        if blocks.is_empty() {
            let (icon, said) = if self.activity.is_loading() {
                (kit.icon(icons::LOADING), "loading…")
            } else {
                ("  ".to_owned(), "nothing yet")
            };
            let line = Line::Text(vec![Span::new(icon, ACCENT), Span::new(said, DIM)]);
            blocks.push((BlockKey::new("empty"), Content::from(vec![line])));
        }
        let unread = self.activity.unread();
        let mut winbar = vec![
            Span::new(" ", Group::NORMAL),
            Span::new(kit.icon(icons::ACTIVITY), ACCENT),
            Span::new("Activity", HEADER),
        ];
        if unread > 0 {
            winbar.push(Span::new(format!("  {unread} unread"), DIM));
        }
        let shown = kit.buffer_mut(buffer);
        shown.doc_mut().set_blocks(blocks);
        shown.set_winbar_spans(Some(winbar));
        effects
    }

    /// An Activity item's lines, `width` cells wide: whether it's unread, its Workspace's icon, what happened, by whom,
    /// where and when; then the gist of its Message, by whom for Replies and DMs.
    fn activity_item(
        &self,
        kit: &Kit,
        entry: &Entry<'_>,
        width: usize,
        now: Timestamp,
    ) -> [Line; 2] {
        let Some(workspace) = self.workspaces.get(entry.workspace) else {
            return [Line::text(""), Line::text("")];
        };
        let item = entry.item;
        let renderer = Renderer::new(self, kit, workspace, entry.workspace, 0);
        let name = |id| workspace.user_name(id).to_owned();
        let place = place_name(workspace, &item.message.conversation);
        let icon = |icon: &str, group| vec![Span::new(kit.icon(icon) + " ", group)];
        let (what, words) = match &item.happened {
            Happened::Mention { author, of } => {
                let whom = match of {
                    MentionOf::User => "you",
                    MentionOf::Group => "your group",
                    MentionOf::Everyone => "everyone",
                };
                let words = format!("{} mentioned {whom} in {place}", name(author));
                (icon(icons::MENTION, MENTION_ME), words)
            }
            Happened::Reaction { user, emoji } => {
                let glyph = match renderer.emoji(emoji) {
                    LayoutPiece::Atom(glyph, cells) => {
                        let pad = EMOJI_CELLS.saturating_sub(usize::from(cells)).max(1);
                        vec![glyph, Span::new(" ".repeat(pad), Group::NORMAL)]
                    }
                    LayoutPiece::Text(_) => icon(icons::REACTION, ACCENT),
                };
                (
                    glyph,
                    format!("{} reacted to your message in {place}", name(user)),
                )
            }
            Happened::Replies { unread } => {
                let words = match unread {
                    0 => format!("Replies in a thread in {place}"),
                    1 => format!("1 new reply in a thread in {place}"),
                    count => format!("{count} new replies in a thread in {place}"),
                };
                (icon(icons::THREAD, ACCENT), words)
            }
            Happened::Dm => (icon(icons::DM, ACCENT), format!("Message from {place}")),
            Happened::Added { author } => (
                icon(icons::ADDED, ACCENT),
                format!("{} added you to {place}", name(author)),
            ),
        };
        let when = when(item.at, &self.zone, now);
        let unread = if item.unread {
            Span::new(kit.icon(icons::UNREAD), ACCENT)
        } else {
            Span::new("  ", DIM)
        };
        let group = if item.unread { UNREAD } else { READ };
        let room = width.saturating_sub(ACTIVITY_LEAD + when.width() + 1);
        let mut first = vec![unread, workspace_marker(entry.workspace, workspace)];
        first.extend(what);
        first.extend(cut(&words, group, room));
        let used: usize = first.iter().map(|span| span.text.width()).sum();
        first.push(Span::new(
            " ".repeat(width.saturating_sub(used + when.width())),
            Group::NORMAL,
        ));
        first.push(Span::new(when, DIM));

        let gist = match self.activity_message(entry.workspace, &item.message) {
            Fetched::Got(message) => {
                let by = match item.happened {
                    Happened::Replies { .. } | Happened::Dm => {
                        format!("{}: ", workspace.author_name(&message.author))
                    }
                    Happened::Mention { .. }
                    | Happened::Reaction { .. }
                    | Happened::Added { .. } => String::new(),
                };
                by + &workspace.gist(&message.content)
            }
            Fetched::Unasked | Fetched::Fetching => "…".to_owned(),
            Fetched::Unavailable => "message unavailable".to_owned(),
        };
        let mut second = vec![Span::new(" ".repeat(ACTIVITY_LEAD), Group::NORMAL)];
        second.extend(cut(&gist, DIM, width.saturating_sub(ACTIVITY_LEAD)));
        [Line::Text(first), Line::Text(second)]
    }

    /// Draws `place`'s Messages in `buffer`, wrapped to its window's width, and its winbar; and lets the window
    /// follow the buffer's end once its cursor landed where it opened.
    pub(super) fn render_place(&mut self, kit: &mut Kit, buffer: BufferId, place: &Place) {
        let width = kit.text_width(buffer);
        self.widths.insert(buffer, width);
        let blocks = self.place_blocks(kit, place, width);
        let winbar = self.winbar(kit, place);
        // A DM's name may have arrived since the buffer was named after its id.
        let name = self.buffer_name(place);
        let shown = kit.buffer_mut(buffer);
        shown.set_name(name);
        shown.doc_mut().set_blocks(blocks);
        shown.set_winbar_spans(Some(winbar));
        if let Some(key) = self.landing.get(&buffer) {
            let loaded = self
                .workspaces
                .get(place.conversation.workspace)
                .and_then(|workspace| workspace.timeline(place))
                .is_some();
            let reaching =
                place.thread.is_none() && self.reaching.contains_key(&place.conversation);
            if kit.buffer(buffer).doc().contains(key.as_str()) || (loaded && !reaching) {
                self.landing.remove(&buffer);
                kit.buffer_mut(buffer).set_follows_end(true);
            }
        }
    }

    /// `place`'s Messages as blocks, each keyed by its timestamp, followed by those still being sent there. A
    /// Thread's parent comes first, then a rule counting its Replies. What's new starts under a rule of its own, in
    /// a block of its own: what opening it lands on.
    fn place_blocks(
        &self,
        kit: &Kit,
        place: &Place,
        width: u16,
    ) -> Vec<(BlockKey, Content<Target>)> {
        let loading = || {
            let line = Line::Text(vec![
                Span::new(kit.icon(icons::LOADING), ACCENT),
                Span::new("loading…", DIM),
            ]);
            vec![(BlockKey::new("loading"), Content::from(vec![line]))]
        };
        let key = place.conversation.workspace;
        let Some(workspace) = self.workspaces.get(key) else {
            return loading();
        };
        let Some(timeline) = workspace.timeline(place) else {
            return loading();
        };
        let in_thread = place.thread.is_some();
        let replies = timeline.messages().len().saturating_sub(1);
        let new_after = self.new_after.get(place).copied();
        let rule = |text: String, group| Line::Text(vec![Span::new(centered(&text, width), group)]);
        let mut blocks = Vec::with_capacity(timeline.messages().len() + 2);
        let mut previous: Option<&Message> = None;
        for (index, message) in timeline.messages().enumerate() {
            let day = day(message.ts, &self.zone);
            let new_day = previous.is_none_or(|p| day != self::day(p.ts, &self.zone));
            let first_reply = in_thread && index == 1;
            let first_new = new_after.is_some_and(|after| {
                message.ts > after && previous.is_none_or(|previous| previous.ts <= after)
            });
            let target = Target::Message(place.clone(), message.ts);
            let mut lines = Vec::new();
            if new_day
                && day.is_some()
                && let Some(posted) = zoned(message.ts, &self.zone)
            {
                lines.push(rule(
                    format!("─── {} ───", wcgw_kit::when::day(&posted)),
                    DIM,
                ));
            }
            if first_reply {
                let noun = if replies == 1 { "reply" } else { "replies" };
                lines.push(rule(format!("─── {replies} {noun} ───"), DIM));
            }
            if first_new {
                lines.push(rule("─── new ───".to_owned(), RULE));
                blocks.push((
                    BlockKey::new(NEW),
                    Content {
                        lines: std::mem::take(&mut lines),
                        folds: Vec::new(),
                        target: Some(target.clone()),
                    },
                ));
            }
            // An ephemeral Message keeps the header that says so.
            let grouped = !new_day
                && !first_reply
                && !first_new
                && previous.is_some_and(|p| {
                    p.author == message.author
                        && p.ephemeral == message.ephemeral
                        && message.ts.as_micros().saturating_sub(p.ts.as_micros()) < GROUP_WITHIN
                });
            let mut renderer = Renderer::new(self, kit, workspace, key, width);
            renderer.in_thread = in_thread;
            renderer.place = Some(place);
            lines.extend(renderer.message(message, grouped));
            blocks.push((
                BlockKey::new(message.ts.to_string()),
                Content {
                    lines,
                    folds: Vec::new(),
                    target: Some(target),
                },
            ));
            previous = Some(message);
        }
        if in_thread && replies == 0 {
            let none = rule("no replies yet".to_owned(), DIM);
            blocks.push((BlockKey::new("no replies"), Content::from(vec![none])));
        }
        // Messages on their way follow, dimmed, under the user's name.
        for outgoing in self
            .outbox
            .iter()
            .filter(|outgoing| &outgoing.place == place)
        {
            let renderer = Renderer::new(self, kit, workspace, key, width);
            let lines = renderer.sending(&outgoing.written.text);
            blocks.push((
                BlockKey::new(format!("sending {}", outgoing.id)),
                Content::from(lines),
            ));
        }
        blocks
    }

    /// What windows showing `place` say above it: its Conversation, a Thread's too, and with the sidebar hidden, the
    /// section it's listed in, breadcrumb-style.
    fn winbar(&self, kit: &Kit, place: &Place) -> Vec<Span> {
        let conversation = &place.conversation;
        let Some((workspace, found)) = self.workspaces.conversation(conversation) else {
            return vec![Span::new(conversation.id.to_string(), HEADER)];
        };
        let (icon, name) = label(workspace, found);
        let section = self
            .sidebar
            .section_of(conversation)
            .cloned()
            .unwrap_or(Section::Workspace(conversation.workspace));
        let sidebar_shown = self.sidebar_window(kit).is_some();
        let mut bar = vec![Span::new(" ", Group::NORMAL)];
        if !sidebar_shown {
            let (icon, title) = self.section_title(kit, &section);
            bar.extend([
                icon,
                Span::new(title, HEADER),
                Span::new(format!(" {}", kit.icon(icons::CRUMB)), DIM),
            ]);
        }
        // The Workspace's icon says where the Conversation is from, unless the section leading the bar does.
        let marker = (sidebar_shown || matches!(section, Section::VirtualWorkspace(_)))
            .then(|| workspace_marker(conversation.workspace, workspace));
        match place.thread {
            None => {
                bar.extend(marker);
                bar.extend([Span::new(kit.icon(icon), ACCENT), Span::new(name, HEADER)]);
            }
            Some(_) => {
                bar.extend([
                    Span::new(kit.icon(icons::THREAD), ACCENT),
                    Span::new("thread", HEADER),
                    Span::new(" in ", DIM),
                ]);
                bar.extend(marker);
                bar.push(Span::new(format!("{}{name}", kit.icon(icon)), DIM));
            }
        }
        bar
    }

    /// The card `K` shows about the Message at `ts`, held at `place`: who wrote it and when, who reacted, the
    /// buttons an app put in it, and where it leads.
    pub(super) fn details_card(
        &self,
        kit: &Kit,
        place: &Place,
        ts: Ts,
    ) -> Option<Document<Target>> {
        let key = place.conversation.workspace;
        let (workspace, message) = self.message(place, ts)?;
        let renderer = Renderer::new(self, kit, workspace, key, DETAILS_WIDTH);
        let mut header = renderer.header(&message.author);
        if let Author::User(id) = &message.author
            && let Some(user) = workspace.users.get(id)
        {
            for detail in [&user.full_name, &user.title] {
                if !detail.is_empty() && *detail != user.name {
                    header.push(Span::new(format!(" · {detail}"), DIM));
                }
            }
            let theirs = user.time_zone.as_deref();
            if let Some(time) =
                theirs.and_then(|theirs| their_time(theirs, &self.zone, Timestamp::now()))
            {
                header.push(Span::new(format!(" · {time} for them"), DIM));
            }
            match workspace.presence(id) {
                Some(Presence::Active) => header.push(Span::new(" · active", ACTIVE)),
                Some(Presence::Away) => header.push(Span::new(" · away", DIM)),
                None => {}
            }
        }
        let mut posted = zoned(message.ts, &self.zone)
            .map(|z| wcgw_kit::when::long(&z))
            .unwrap_or_default();
        if message.edited {
            posted.push_str(" · edited");
        }
        if message.ephemeral {
            posted.push_str(" · only visible to you");
        }
        let mut lines = vec![
            Line::Text(header),
            Line::Text(vec![Span::new(format!("   {posted}"), DIM)]),
        ];
        if !message.reactions.is_empty() {
            lines.push(Line::text(""));
            lines.extend(
                message
                    .reactions
                    .iter()
                    .map(|reaction| Line::Text(renderer.reactors(reaction, DETAILS_WIDTH))),
            );
        }
        let choices = workspace.choices(&place.conversation.id, message);
        if !choices.is_empty() {
            lines.push(Line::text(""));
        }
        for choice in choices {
            let spans = match choice {
                Choice::Press(button) => {
                    let target = Target::Press {
                        place: place.clone(),
                        ts,
                        button: button.clone(),
                    };
                    vec![button_span(&button).with_target(target)]
                }
                Choice::Open(link) => {
                    let icon = match link.kind {
                        LinkKind::Web => icons::LINK,
                        LinkKind::File => icons::FILE,
                        LinkKind::Message => icons::SLACK,
                    };
                    let target = Target::Url(link.url.clone());
                    let mut spans = vec![
                        Span::new(kit.icon(icon), ACCENT),
                        Span::new(link.label.clone(), Group::NORMAL).with_target(target),
                    ];
                    if link.kind == LinkKind::Web && link.label != link.url {
                        spans.push(Span::new(format!("  {}", host(&link.url)), DIM));
                    }
                    spans
                }
            };
            lines.push(Line::Text(spans));
        }
        let mut doc = Document::new();
        doc.push_back("details", lines);
        Some(doc)
    }

    /// Keeps the card `K` shows current, as names and reactions arrive, and asks for the names of the first people
    /// who reacted with each emoji; forgets it once it's gone.
    pub(super) fn refresh_details(&mut self, kit: &mut Kit) -> Effects {
        let mut effects = Vec::new();
        let Some((place, ts)) = self.details.clone() else {
            return effects;
        };
        if kit.hover_mut().is_none() {
            self.details = None;
            return effects;
        }
        let Some(card) = self.details_card(kit, &place, ts) else {
            kit.close_hover();
            self.details = None;
            return effects;
        };
        if let Some(hover) = kit.hover_mut() {
            *hover = card;
        }
        let key = place.conversation.workspace;
        let reactors: Vec<slackers_model::UserId> = self
            .message(&place, ts)
            .map(|(_, message)| {
                message
                    .reactions
                    .iter()
                    .flat_map(|reaction| reaction.users.iter().take(NAMED_REACTORS))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if let Some(workspace) = self.workspaces.get_mut(key) {
            workspace.want_users(key, reactors, &mut effects);
        }
        effects
    }

    /// Asks for everyone who reacted to the Message at `ts`, held at `place`, when its history named only some.
    pub(super) fn want_reactors(&self, place: &Place, ts: Ts) -> Effects {
        let Some((_, message)) = self.message(place, ts) else {
            return Vec::new();
        };
        if !message
            .reactions
            .iter()
            .any(|reaction| reaction.unnamed() > 0)
        {
            return Vec::new();
        }
        let request = Request::Reactions {
            conversation: place.conversation.id.clone(),
            ts,
        };
        vec![slack(place.conversation.workspace, request)]
    }
}

/// What a sidebar row says about unread Messages: how many Mentions there are, or whether there are any at all.
#[derive(Clone, Copy, Debug, Default)]
struct Badge {
    unread: bool,
    mentions: u32,
}

impl Badge {
    fn of(unread: Option<&Unread>) -> Self {
        Self {
            unread: unread.is_some_and(Unread::has_unread),
            mentions: unread.map_or(0, Unread::mentions),
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
    fn spans(self, kit: &Kit) -> Vec<Span> {
        match (self.mentions, self.unread) {
            (0, false) => Vec::new(),
            (0, true) => vec![Span::new(kit.icon(icons::UNREAD), ACCENT)],
            (mentions, _) => vec![Span::new(
                format!("{}{mentions}", kit.icon(icons::MENTION)),
                MENTION_ME,
            )],
        }
    }
}

/// One sidebar row, `width` cells wide: `lead`, [`LEAD`] cells wide; `name` in `group`, cut to fit with an
/// ellipsis; and on the right, `badge`.
fn entry(
    mut lead: Vec<Span>,
    name: &str,
    group: Group,
    badge: Vec<Span>,
    width: usize,
) -> Vec<Span> {
    let badge_width: usize = badge.iter().map(|span| span.text.width()).sum();
    let room = width.saturating_sub(LEAD + badge_width + 1);
    let name = cut(name, group, room);
    let used: usize = name.iter().map(|span| span.text.width()).sum();
    lead.extend(name);
    lead.push(Span::new(
        " ".repeat(room.saturating_sub(used) + 1),
        Group::NORMAL,
    ));
    lead.extend(badge);
    lead
}

/// As much of `text` in `group` as fits in `room` cells, ending with an ellipsis if it doesn't all fit.
fn cut(text: &str, group: Group, room: usize) -> Vec<Span> {
    if text.width() <= room {
        return vec![Span::new(text, group)];
    }
    let mut kept = String::new();
    let mut used = 0;
    for c in text.chars() {
        let cells = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cells > room.saturating_sub(1) {
            break;
        }
        kept.push(c);
        used += cells;
    }
    kept.push('…');
    vec![Span::new(kept, group)]
}

/// The Workspace at `key`'s icon, two cells wide: its picture, or its initial without one.
fn workspace_marker(key: WorkspaceKey, workspace: &Workspace) -> Span {
    match &workspace.icon {
        Some(url) => Span::picture(picture_source(key, url), 2),
        None => {
            let initial: String = workspace
                .name
                .chars()
                .next()
                .map(char::to_uppercase)
                .into_iter()
                .flatten()
                .collect();
            Span::new(format!("{initial:<1} "), crate::hl::author(&workspace.name))
        }
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

/// What to call where `conversation` is: `#name` for a Channel, else the DM's name.
fn place_name(workspace: &Workspace, conversation: &slackers_model::ConversationId) -> String {
    match workspace.conversation(conversation) {
        Some(
            found @ Conversation {
                kind: ConversationKind::Channel { .. },
                ..
            },
        ) => format!("#{}", found.name),
        Some(found) => workspace.conversation_name(found).to_owned(),
        None => "a conversation you're not in".to_owned(),
    }
}

/// `text` with spaces before it to stand in the middle of `width` cells.
fn centered(text: &str, width: u16) -> String {
    let pad = usize::from(width).saturating_sub(text.width()) / 2;
    format!("{}{text}", " ".repeat(pad))
}

/// Lays out one Message, as lines of the kit's.
///
/// A Message hangs off its author's avatar: the avatar and a space fill the [`GUTTER`] on the header line, and
/// everything else is indented by it.
struct Renderer<'a> {
    app: &'a Slackers,
    kit: &'a Kit,
    workspace: &'a Workspace,
    /// Where `workspace` is, to fetch its pictures through.
    key: WorkspaceKey,
    /// Room for content, right of the gutter, in cells.
    width: u16,
    /// Whether it lays out a Thread, whose buffer counts the Replies itself.
    in_thread: bool,
    /// Where the Messages are, for their buttons to press.
    place: Option<&'a Place>,
}

impl<'a> Renderer<'a> {
    /// A renderer for lines `width` cells wide, gutter included, of Messages in `workspace`, which is at `key`.
    fn new(
        app: &'a Slackers,
        kit: &'a Kit,
        workspace: &'a Workspace,
        key: WorkspaceKey,
        width: u16,
    ) -> Self {
        Self {
            app,
            kit,
            workspace,
            key,
            width: width.saturating_sub(GUTTER),
            in_thread: false,
            place: None,
        }
    }

    /// `message`'s lines. A `grouped` one follows another by the same author: it gets no header, just a faint dot
    /// in the gutter where it starts.
    fn message(&mut self, message: &Message, grouped: bool) -> Vec<Line> {
        let zone = &self.app.zone;
        let mut lines = Vec::new();
        if !grouped {
            let mut header = self.header(&message.author);
            header.push(Span::new(format!("  {}", time(message.ts, zone)), DIM));
            if message.edited {
                header.push(Span::new(format!(" {}", self.kit.icon(icons::EDITED)), DIM));
            }
            if message.ephemeral {
                header.extend([
                    Span::new(format!("  {}", self.kit.icon(icons::EPHEMERAL)), DIM),
                    Span::new("only visible to you", DIM),
                ]);
            }
            lines.push(Line::Text(header));
        }
        let mut content = self.text(&message.content, message.ts);
        if grouped
            && message.edited
            && let Some(Line::Text(last)) = content.last_mut()
        {
            last.push(Span::new(format!(" {}", self.kit.icon(icons::EDITED)), DIM));
        }
        for file in &message.files {
            content.extend(self.file(file));
        }
        if !message.reactions.is_empty() {
            let mut pieces = Vec::new();
            for reaction in &message.reactions {
                pieces.push(self.emoji(&reaction.emoji));
                let group = if reaction.users.contains(&self.workspace.me) {
                    MENTION
                } else {
                    DIM
                };
                pieces.push(Piece::Text(Span::new(
                    format!(" {}  ", reaction.count),
                    group,
                )));
            }
            content.extend(layout::wrap(pieces, self.width).into_iter().map(Line::Text));
        }
        if let Some(thread) = message.thread.filter(|_| !self.in_thread) {
            content.push(Line::Text(vec![
                Span::new(self.kit.icon(icons::THREAD), ACCENT),
                Span::new(thread.replies.to_string(), ACCENT),
                Span::new(format!("  {}", time(thread.latest, zone)), DIM),
            ]));
        }
        let mut first = true;
        lines.extend(content.into_iter().map(|line| match line {
            Line::Text(mut spans) => {
                let gutter = if grouped && first { " · " } else { "   " };
                first = false;
                spans.insert(0, Span::new(gutter, DIM));
                Line::Text(spans)
            }
            image @ Line::Image { .. } => image,
        }));
        lines
    }

    /// The lines of `text`, posted by the user and not yet confirmed: dimmed, under a header saying so.
    fn sending(&self, text: &str) -> Vec<Line> {
        let mut header = self.header(&Author::User(self.workspace.me.clone()));
        header.extend([
            Span::new("  ", DIM),
            Span::new(self.kit.icon(icons::SENDING), DIM),
            Span::new("sending", DIM),
        ]);
        let mut lines = vec![Line::Text(header)];
        let pieces = vec![Piece::Text(Span::new(text, DIM))];
        lines.extend(
            layout::wrap(pieces, self.width)
                .into_iter()
                .map(|mut spans| {
                    spans.insert(0, Span::new("   ", DIM));
                    Line::Text(spans)
                }),
        );
        lines
    }

    /// The start of a header line: `author`'s avatar in the gutter, then their name.
    fn header(&self, author: &Author) -> Vec<Span> {
        let group = match author {
            Author::User(id) => crate::hl::author(id.as_str()),
            Author::Bot { .. } | Author::UnnamedBot(_) => BOT,
        };
        let name = self.workspace.author_name(author).to_owned();
        vec![
            self.avatar(author, group),
            Span::new(" ", Group::NORMAL),
            Span::new(name, group),
        ]
    }

    /// `author`'s avatar, two cells wide: their picture, or an icon in `group` without one.
    fn avatar(&self, author: &Author, group: Group) -> Span {
        match self.workspace.avatar(author) {
            Some(url) => Span::picture(picture_source(self.key, url), 2),
            None => {
                let icon = match author {
                    Author::User(_) => icons::AVATAR,
                    Author::Bot { .. } | Author::UnnamedBot(_) => icons::BOT,
                };
                Span::new(self.kit.icon(icon), group)
            }
        }
    }

    /// A shared file's lines: an image as [`Renderer::image`] draws it; anything else as its name, leading to it.
    fn file(&self, file: &File) -> Vec<Line> {
        if let Some(image) = &file.image {
            return vec![self.image(image)];
        }
        let mut line = labelled(self.kit.icon(icons::FILE), &file.name);
        if let Some(link) = file.link.as_ref().filter(|link| is_web(link)) {
            line = line
                .into_iter()
                .map(|span| span.with_target(Target::Url(link.clone())))
                .collect();
        }
        vec![Line::Text(line)]
    }

    /// An image: its picture, at its own size or shrunk to [`IMAGE`]; the kit says how it loads.
    fn image(&self, image: &Image) -> Line {
        let (cols, rows) = fitting(image.width, image.height, self.kit.cell_size());
        Line::Image {
            rows,
            cols,
            source: picture_source(self.key, &image.url),
        }
    }

    fn text(&self, text: &RichText, ts: Ts) -> Vec<Line> {
        let mut lines = Vec::new();
        for block in &text.blocks {
            match block {
                TextBlock::Paragraph(inlines) => {
                    let pieces = self.pieces(inlines);
                    lines.extend(layout::wrap(pieces, self.width).into_iter().map(Line::Text));
                }
                TextBlock::Quote(inlines) => {
                    let pieces = self.pieces(inlines);
                    lines.extend(self.prefixed(pieces, Span::new("▎ ", QUOTE), "  "));
                }
                TextBlock::Preformatted(code) => {
                    for line in code.lines() {
                        let pieces = vec![Piece::Text(Span::new(line, CODE))];
                        lines.extend(self.prefixed(pieces, Span::new("▏ ", DIM), "▏ "));
                    }
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
                        lines.extend(self.prefixed(pieces, Span::new(marker, DIM), &indent));
                    }
                }
                TextBlock::Image { image, .. } => lines.push(self.image(image)),
                TextBlock::Actions(buttons) => {
                    let mut pieces = Vec::new();
                    for (index, button) in buttons.iter().enumerate() {
                        if index > 0 {
                            pieces.push(Piece::Text(Span::new("  ", Group::NORMAL)));
                        }
                        let mut span = button_span(button);
                        if let Some(place) = self.place {
                            span = span.with_target(Target::Press {
                                place: place.clone(),
                                ts,
                                button: button.clone(),
                            });
                        }
                        let cells = u16::try_from(span.text.width()).unwrap_or(u16::MAX);
                        pieces.push(Piece::Atom(span, cells));
                    }
                    lines.extend(layout::wrap(pieces, self.width).into_iter().map(Line::Text));
                }
            }
        }
        lines
    }

    /// `pieces` wrapped to leave room for `first` before the first line and `rest` (as wide) before the others.
    fn prefixed(&self, pieces: Vec<Piece>, first: Span, rest: &str) -> Vec<Line> {
        let room = self
            .width
            .saturating_sub(u16::try_from(first.text.width()).unwrap_or(0));
        layout::wrap(pieces, room)
            .into_iter()
            .enumerate()
            .map(|(index, mut spans)| {
                let prefix = if index == 0 {
                    first.clone()
                } else {
                    Span::new(rest, first.group)
                };
                spans.insert(0, prefix);
                Line::Text(spans)
            })
            .collect()
    }

    fn pieces(&self, inlines: &[Inline]) -> Vec<Piece> {
        let workspace = self.workspace;
        inlines
            .iter()
            .map(|inline| match inline {
                Inline::Text { text, style } => Piece::Text(styled(text, *style)),
                Inline::Emoji(emoji) => self.emoji(emoji),
                Inline::User(id) => {
                    let group = if *id == workspace.me {
                        MENTION_ME
                    } else {
                        MENTION
                    };
                    let name = format!("@{}", workspace.user_name(id));
                    Piece::Text(
                        Span::new(name, group).with_target(Target::Person(self.key, id.clone())),
                    )
                }
                Inline::Channel(id) => {
                    let known = workspace.conversation(id);
                    let name = format!("#{}", known.map_or(id.as_str(), |c| c.name.as_str()));
                    let span = Span::new(name, LINK);
                    Piece::Text(match known {
                        Some(_) => span.with_target(Target::Conversation(ConversationRef {
                            workspace: self.key,
                            id: id.clone(),
                        })),
                        None => span,
                    })
                }
                Inline::Broadcast(broadcast) => {
                    Piece::Text(Span::new(broadcast_name(*broadcast), MENTION_ME))
                }
                Inline::UserGroup(_) => Piece::Text(Span::new("@group", MENTION)),
                Inline::Link { url, label } => {
                    let shown = label.clone().unwrap_or_else(|| url.clone());
                    let span = Span::new(shown, LINK);
                    Piece::Text(if is_web(url) {
                        span.with_target(Target::Url(url.clone()))
                    } else {
                        span
                    })
                }
            })
            .collect()
    }

    /// A reaction's line in a Message's details, `width` cells wide: its emoji, how many reacted, and who, "you"
    /// for the user.
    fn reactors(&self, reaction: &Reaction, width: u16) -> Vec<Span> {
        let mut spans = Vec::new();
        let mut used = 0;
        let count = Piece::Text(Span::new(format!(" {}  ", reaction.count), DIM));
        for piece in [self.emoji(&reaction.emoji), count] {
            let (span, cells) = match piece {
                LayoutPiece::Atom(span, cells) => (span, usize::from(cells)),
                LayoutPiece::Text(span) => {
                    let cells = span.text.width();
                    (span, cells)
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
        spans.push(Span::new(who(&names, count, room), Group::NORMAL));
        spans
    }

    /// `emoji`, as a piece of a line: its characters, its picture two cells wide, or its `:name:`.
    fn emoji(&self, emoji: &Emoji) -> Piece {
        match self.workspace.emojis.drawable(emoji) {
            Drawable::Text(text) => Piece::Atom(
                Span::new(text, Group::NORMAL),
                u16::try_from(text.width().max(1)).unwrap_or(2),
            ),
            Drawable::Picture(url) => {
                Piece::Atom(Span::picture(picture_source(self.key, url), 2), 2)
            }
            Drawable::Name => Piece::Text(Span::new(format!(":{}:", emoji.name), Group::NORMAL)),
        }
    }
}

/// Plain text of a Message in `style`: code in its group, the rest in the window's, bold, italic or struck through.
fn styled(text: &str, style: TextStyle) -> Span {
    let group = if style.contains(TextStyle::CODE) {
        CODE
    } else {
        Group::NORMAL
    };
    let mut modifiers = Modifier::empty();
    for (flag, modifier) in [
        (TextStyle::BOLD, Modifier::BOLD),
        (TextStyle::ITALIC, Modifier::ITALIC),
        (TextStyle::STRIKE, Modifier::CROSSED_OUT),
    ] {
        if style.contains(flag) {
            modifiers |= modifier;
        }
    }
    let span = Span::new(text, group);
    if modifiers.is_empty() {
        span
    } else {
        span.with_style(Style::new().add_modifier(modifiers))
    }
}

/// An app's button, as it shows: its label in brackets, the one the app suggests standing out.
fn button_span(button: &Button) -> Span {
    let group = if button.primary {
        PRIMARY_BUTTON
    } else {
        BUTTON
    };
    Span::new(format!("[ {} ]", button.label), group)
}

/// A dim line: `icon`, then `label`.
fn labelled(icon: String, label: &str) -> Vec<Span> {
    vec![Span::new(icon, DIM), Span::new(label, DIM)]
}

/// The cells, across and down, a `width` × `height` pixel picture takes at its own size on cells of `cell` pixels,
/// shrunk, keeping its shape, to fit in [`IMAGE`]. A picture whose size isn't known takes all of it.
fn fitting(width: u32, height: u32, cell: (u16, u16)) -> (u16, u16) {
    let (max_cols, max_rows) = IMAGE;
    if width == 0 || height == 0 {
        return IMAGE;
    }
    let (cell_width, cell_height) = (f64::from(cell.0.max(1)), f64::from(cell.1.max(1)));
    let scale = f64::min(
        1.0,
        f64::min(
            f64::from(max_cols) * cell_width / f64::from(width),
            f64::from(max_rows) * cell_height / f64::from(height),
        ),
    );
    let cells = |pixels: u32, per_cell: f64, limit: u16| {
        let cells = (f64::from(pixels) * scale / per_cell).ceil();
        // `as` saturates; the clamp keeps the box within the limit despite rounding up.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cells = cells as u16;
        cells.clamp(1, limit)
    };
    (
        cells(width, cell_width, max_cols),
        cells(height, cell_height, max_rows),
    )
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
    (there.offset() != here.offset()).then(|| wcgw_kit::when::clock(&there))
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

pub(super) fn zoned(ts: Ts, zone: &TimeZone) -> Option<jiff::Zoned> {
    let micros = i64::try_from(ts.as_micros()).ok()?;
    Some(
        Timestamp::from_microsecond(micros)
            .ok()?
            .to_zoned(zone.clone()),
    )
}

/// The local time of day `ts` was posted, as `HH:MM`.
pub(super) fn time(ts: Ts, zone: &TimeZone) -> String {
    zoned(ts, zone)
        .map(|z| wcgw_kit::when::clock(&z))
        .unwrap_or_default()
}

/// The local date `ts` was posted on.
fn day(ts: Ts, zone: &TimeZone) -> Option<Date> {
    zoned(ts, zone).map(|z| z.date())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_reacted_names_as_many_as_fit_and_counts_the_rest() {
        assert_eq!(who(&["Ada", "Bob"], 2, 40), "Ada, Bob");
        assert_eq!(who(&["Ada", "Bob"], 5, 40), "Ada, Bob and 3 others");
        assert_eq!(who(&["Ada", "Bob"], 5, 18), "Ada and 4 others");
        assert_eq!(who(&["Ada"], 2, 3), "2 people");
        assert_eq!(who(&[], 1, 40), "1 person");
    }

    #[test]
    fn someone_elses_time_shows_only_when_it_differs_from_ours() {
        let now = Timestamp::from_second(1_700_000_000).unwrap();
        let utc = TimeZone::UTC;
        assert_eq!(their_time("UTC", &utc, now), None);
        assert_eq!(
            their_time("Asia/Tokyo", &utc, now).as_deref(),
            Some("07:13")
        );
        assert_eq!(their_time("Not/AZone", &utc, now), None);
    }

    #[test]
    fn a_picture_takes_its_own_size_up_to_the_room_for_images() {
        assert_eq!(fitting(100, 40, (10, 20)), (10, 2));
        assert_eq!(fitting(1200, 280, (10, 20)), (60, 7));
        assert_eq!(fitting(0, 0, (10, 20)), IMAGE);
    }

    #[test]
    fn a_picture_names_the_workspace_it_comes_through() {
        let source = picture_source(WorkspaceKey::nth(3), "https://a b");
        assert_eq!(
            picture_of(&source),
            Some((WorkspaceKey::nth(3), "https://a b"))
        );
        assert_eq!(picture_of("https://x"), None);
    }
}
