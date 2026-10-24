//! The app's state, and how events change it. No I/O here: every effect is a [`Command`] for the runtime to carry
//! out, so that `update` can be tested on its own.
//!
//! Keys work as in vim: Normal mode moves around, `i` enters Insert mode to write in the open Conversation, and
//! `:` enters Command mode (`:q` quits).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::TextArea;
use slackers_gfx::{Fit, Picture};
use slackers_model::{
    Activity, Author, Conversation, ConversationId, ConversationKind, Inline, Message, Timeline,
    UserId,
};
use slackers_slack::{Connection, Error, LiveEvent, Request, Snapshot, Update};

use crate::emoji::Emojis;
use crate::search::{Finder, Hit};

/// Something that happened, for [`App::update`] to react to.
#[derive(Debug)]
pub(crate) enum Event {
    Terminal(TerminalEvent),
    Slack(Update),
    /// A picture finished loading and is in the terminal.
    PictureReady {
        url: String,
        picture: Picture,
    },
    /// A picture couldn't be loaded.
    PictureFailed {
        url: String,
    },
    /// The terminal lost every picture, as it may when slackers hands it to another program.
    PicturesLost,
    /// The external editor asked for with [`Command::Edit`] finished: with the new draft, or why there's none.
    Edited {
        conversation: ConversationId,
        outcome: Result<String, String>,
    },
    /// What the last draw found; see [`Drawn`].
    Drawn(Drawn),
}

/// What a draw found out that only drawing can tell.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Drawn {
    /// Pictures it would have shown, had they been loaded, by URL.
    pub(crate) wanted_pictures: Vec<String>,
    /// How far up the open Timeline can scroll, in lines.
    pub(crate) max_scroll: usize,
    /// How many lines of Timeline fit on screen.
    pub(crate) page: usize,
    /// When something drawn next changes frame, as animation time; `None` when nothing animates.
    pub(crate) next_change: Option<Duration>,
}

/// Something for the runtime to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Slack(Request),
    /// Decode `bytes` and send them to the terminal as the picture for `url`, then report back with
    /// [`Event::PictureReady`] or [`Event::PictureFailed`].
    Prepare {
        url: String,
        bytes: Vec<u8>,
        fit: Fit,
    },
    /// Hand the terminal to `$EDITOR` to edit `text`, the draft for `conversation`, then report back with
    /// [`Event::Edited`] and [`Event::PicturesLost`].
    Edit {
        conversation: ConversationId,
        text: String,
    },
    Quit,
}

/// What keys do, as in vim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Keys move around the pane in [`Focus`].
    Normal,
    /// Keys write the open Conversation's draft.
    Insert,
    /// Keys write a command, shown after `:` on the status line.
    Command(String),
    /// Keys narrow the sidebar down to the Conversations matching a query, shown after `/` on the status line.
    Search(Search),
}

/// A search of the sidebar under way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Search {
    pub(crate) query: String,
    /// The matching Conversations, best first; each [`Hit::index`] is a position in the sidebar.
    pub(crate) hits: Vec<Hit>,
    /// The hit under the cursor.
    pub(crate) selected: usize,
}

/// Which pane Normal mode's keys move around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Sidebar,
    Timeline,
}

/// Where signing in stands.
#[derive(Debug)]
pub(crate) enum Phase {
    SigningIn,
    Ready(Box<Workspace>),
    Failed(String),
}

/// A signed-in Workspace, as the user sees it.
#[derive(Debug)]
pub(crate) struct Workspace {
    pub(crate) name: String,
    pub(crate) me: UserId,
    /// The sidebar, in display order: Channels by name, then DMs, most recently active first.
    pub(crate) conversations: Vec<Conversation>,
    pub(crate) activity: HashMap<ConversationId, Activity>,
    pub(crate) timelines: HashMap<ConversationId, Timeline>,
    /// Conversations with a history request in flight.
    loading: HashSet<ConversationId>,
}

/// A Message on its way to Slack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outgoing {
    /// Matches the [`Request::Post`] to its [`Update::Posted`].
    id: u64,
    pub(crate) conversation: ConversationId,
    /// As typed.
    pub(crate) text: String,
}

/// Everything slackers shows. Only [`App::update`] changes it; the view reads it.
#[derive(Debug)]
pub(crate) struct App {
    /// The name the user gave the Workspace, until Slack names it.
    pub(crate) label: String,
    pub(crate) phase: Phase,
    pub(crate) connection: Option<Connection>,
    /// The latest thing worth telling the user, usually a problem.
    pub(crate) notice: Option<String>,
    /// People's names, as far as they've been looked up.
    pub(crate) users: HashMap<UserId, String>,
    requested_users: HashSet<UserId>,
    pub(crate) emojis: Emojis,
    pub(crate) mode: Mode,
    /// The sidebar entry under the cursor.
    pub(crate) selected: usize,
    pub(crate) open: Option<ConversationId>,
    pub(crate) focus: Focus,
    /// How many lines the open Timeline is scrolled up from its newest Message.
    pub(crate) scroll: usize,
    /// Unsent text, per Conversation.
    pub(crate) drafts: HashMap<ConversationId, TextArea<'static>>,
    /// Messages posted but not yet confirmed, oldest first.
    pub(crate) outbox: Vec<Outgoing>,
    next_post: u64,
    /// Whether `Ctrl-x` was just pressed in Insert mode, starting `Ctrl-x Ctrl-e`.
    awaiting_chord: bool,
    /// Whether the terminal window has focus. Messages count as read only while it does.
    focused: bool,
    finder: Finder,
    /// The last [`Drawn`] report.
    drawn: Drawn,
    /// When animations started; every picture is drawn against it, so copies of one emoji animate in step.
    epoch: Instant,
}

impl App {
    pub(crate) fn new(label: String) -> Self {
        Self {
            label,
            phase: Phase::SigningIn,
            connection: None,
            notice: None,
            users: HashMap::new(),
            requested_users: HashSet::new(),
            emojis: Emojis::default(),
            mode: Mode::Normal,
            selected: 0,
            open: None,
            focus: Focus::Sidebar,
            scroll: 0,
            drafts: HashMap::new(),
            outbox: Vec::new(),
            next_post: 0,
            awaiting_chord: false,
            focused: true,
            finder: Finder::new(),
            drawn: Drawn::default(),
            epoch: Instant::now(),
        }
    }

    /// Time since animations started.
    pub(crate) fn animation_time(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// When the screen next needs redrawing for animation alone, as animation time.
    pub(crate) fn next_change(&self) -> Option<Duration> {
        self.drawn.next_change
    }

    pub(crate) fn workspace(&self) -> Option<&Workspace> {
        match &self.phase {
            Phase::Ready(workspace) => Some(workspace),
            Phase::SigningIn | Phase::Failed(_) => None,
        }
    }

    /// What to call `user`: their name once looked up, else their id.
    pub(crate) fn user_name<'a>(&'a self, user: &'a UserId) -> &'a str {
        self.users.get(user).map_or(user.as_str(), String::as_str)
    }

    /// What to call `conversation`: a DM after the other person, anything else by its name.
    pub(crate) fn conversation_name<'a>(&'a self, conversation: &'a Conversation) -> &'a str {
        conversation_name(&self.users, conversation)
    }

    /// The open Conversation's draft, if it has one.
    pub(crate) fn draft(&self) -> Option<&TextArea<'static>> {
        self.drafts.get(self.open.as_ref()?)
    }

    /// Reacts to `event`, returning what the runtime should do.
    pub(crate) fn update(&mut self, event: Event) -> Vec<Command> {
        let mut commands = Vec::new();
        match event {
            Event::Terminal(TerminalEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                self.key(key, &mut commands);
            }
            Event::Terminal(TerminalEvent::FocusGained) => self.focused = true,
            Event::Terminal(TerminalEvent::FocusLost) => self.focused = false,
            Event::Terminal(_) => {}
            Event::Slack(update) => self.slack(update, &mut commands),
            Event::PictureReady { url, picture } => self.emojis.loaded(url, picture),
            Event::PictureFailed { url } => self.emojis.failed(url),
            Event::PicturesLost => self.emojis.forget_pictures(),
            Event::Edited {
                conversation,
                outcome,
            } => match outcome {
                Ok(text) => {
                    self.drafts.insert(conversation, draft_of(&text));
                }
                Err(reason) => self.notice = Some(reason),
            },
            Event::Drawn(drawn) => {
                for url in &drawn.wanted_pictures {
                    if self.emojis.request(url) {
                        commands.push(Command::Slack(Request::Download { url: url.clone() }));
                    }
                }
                self.scroll = self.scroll.min(drawn.max_scroll);
                self.drawn = drawn;
                if self.scroll == self.drawn.max_scroll {
                    self.load_older(&mut commands);
                }
            }
        }
        self.mark_seen(&mut commands);
        // People's names keep arriving: keep a search's hits current.
        self.refresh_search();
        commands
    }

    fn key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        match self.mode {
            Mode::Normal => self.normal_key(key, commands),
            Mode::Insert => self.insert_key(key, commands),
            Mode::Command(_) => self.command_key(key, commands),
            Mode::Search(_) => self.search_key(key, commands),
        }
    }

    fn normal_key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, control) {
            (KeyCode::Char('c'), true) => {
                self.notice = Some("type :q and press Enter to quit".to_owned());
                return;
            }
            (KeyCode::Char(':'), false) => {
                self.mode = Mode::Command(String::new());
                return;
            }
            (KeyCode::Char('/'), false) => {
                self.mode = Mode::Search(Search {
                    query: String::new(),
                    hits: Vec::new(),
                    selected: 0,
                });
                self.focus = Focus::Sidebar;
                return;
            }
            (KeyCode::Char('i'), false) => {
                if let Some(open) = self.open.clone() {
                    self.drafts.entry(open).or_insert_with(|| draft_of(""));
                    self.mode = Mode::Insert;
                    self.focus = Focus::Timeline;
                    self.scroll = 0;
                } else {
                    self.notice = Some("open a conversation first".to_owned());
                }
                return;
            }
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Timeline,
                    Focus::Timeline => Focus::Sidebar,
                };
                return;
            }
            _ => {}
        }
        match self.focus {
            Focus::Sidebar => match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.select(self.selected.saturating_add(1)),
                KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
                KeyCode::Char('g') | KeyCode::Home => self.select(0),
                KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                    let selected = self
                        .workspace()
                        .and_then(|w| w.conversations.get(self.selected));
                    if let Some(id) = selected.map(|c| c.id.clone()) {
                        self.open(id, commands);
                        self.focus = Focus::Timeline;
                    }
                }
                _ => {}
            },
            Focus::Timeline => match (key.code, control) {
                (KeyCode::Char('u'), true) | (KeyCode::PageUp, _) => {
                    self.scroll_by(self.half_page(), commands);
                }
                (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => {
                    self.scroll_by(-self.half_page(), commands);
                }
                (KeyCode::Char('k') | KeyCode::Up, _) => self.scroll_by(1, commands),
                (KeyCode::Char('j') | KeyCode::Down, _) => self.scroll_by(-1, commands),
                (KeyCode::Char('g') | KeyCode::Home, _) => self.scroll_by(isize::MAX, commands),
                (KeyCode::Char('G') | KeyCode::End, _) => self.scroll = 0,
                (KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc, _) => {
                    self.focus = Focus::Sidebar;
                }
                _ => {}
            },
        }
    }

    fn insert_key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let Some(open) = self.open.clone() else {
            self.mode = Mode::Normal;
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let chord = std::mem::take(&mut self.awaiting_chord);
        match (key.code, control) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => self.mode = Mode::Normal,
            (KeyCode::Char('x'), true) => self.awaiting_chord = true,
            (KeyCode::Char('e'), true) if chord => {
                let text = self.drafts.get(&open).map(|draft| draft.lines().join("\n"));
                commands.push(Command::Edit {
                    conversation: open,
                    text: text.unwrap_or_default(),
                });
            }
            // Shift+Enter needs the kitty keyboard protocol to tell it from Enter; Alt+Enter works without.
            (KeyCode::Enter, _)
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.drafts
                    .entry(open)
                    .or_insert_with(|| draft_of(""))
                    .insert_newline();
            }
            (KeyCode::Enter, _) => self.send(open, commands),
            _ => {
                self.drafts
                    .entry(open)
                    .or_insert_with(|| draft_of(""))
                    .input(key);
            }
        }
    }

    fn command_key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let Mode::Command(line) = &mut self.mode else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, control) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => self.mode = Mode::Normal,
            (KeyCode::Backspace, _) => {
                if line.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            (KeyCode::Enter, _) => {
                let line = std::mem::take(line);
                self.mode = Mode::Normal;
                match line.trim() {
                    "" => {}
                    "q" | "q!" | "qa" | "qa!" | "quit" | "quitall" => commands.push(Command::Quit),
                    other => self.notice = Some(format!("not a command: :{other}")),
                }
            }
            (KeyCode::Char(c), false) => line.push(c),
            _ => {}
        }
    }

    fn search_key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let Mode::Search(search) = &mut self.mode else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, control) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => self.mode = Mode::Normal,
            (KeyCode::Down, _) | (KeyCode::Char('n' | 'j'), true) => {
                search.selected = (search.selected + 1).min(search.hits.len().saturating_sub(1));
            }
            (KeyCode::Up, _) | (KeyCode::Char('p' | 'k'), true) => {
                search.selected = search.selected.saturating_sub(1);
            }
            (KeyCode::Backspace, _) => {
                if search.query.pop().is_none() {
                    self.mode = Mode::Normal;
                } else {
                    search.selected = 0;
                }
            }
            (KeyCode::Enter, _) => {
                let chosen = search.hits.get(search.selected).map(|hit| hit.index);
                let query = std::mem::take(&mut search.query);
                self.mode = Mode::Normal;
                let id = chosen.and_then(|index| {
                    self.selected = index;
                    self.workspace()?
                        .conversations
                        .get(index)
                        .map(|c| c.id.clone())
                });
                match id {
                    Some(id) => {
                        self.open(id, commands);
                        self.focus = Focus::Timeline;
                    }
                    None => self.notice = Some(format!("no conversation matches /{query}")),
                }
            }
            (KeyCode::Char(c), false) => {
                search.query.push(c);
                search.selected = 0;
            }
            _ => {}
        }
    }

    /// Re-ranks the sidebar against the search query, if a search is under way.
    fn refresh_search(&mut self) {
        let Mode::Search(search) = &mut self.mode else {
            return;
        };
        let Phase::Ready(workspace) = &self.phase else {
            return;
        };
        let names = workspace
            .conversations
            .iter()
            .map(|c| conversation_name(&self.users, c));
        search.hits = self.finder.rank(&search.query, names);
        search.selected = search.selected.min(search.hits.len().saturating_sub(1));
    }

    /// Posts the draft for `conversation`, unless it's blank, and empties it.
    fn send(&mut self, conversation: ConversationId, commands: &mut Vec<Command>) {
        let Some(draft) = self.drafts.get_mut(&conversation) else {
            return;
        };
        let text = draft.lines().join("\n");
        if text.trim().is_empty() {
            return;
        }
        *draft = draft_of("");
        let id = self.next_post;
        self.next_post += 1;
        self.outbox.push(Outgoing {
            id,
            conversation: conversation.clone(),
            text: text.clone(),
        });
        self.scroll = 0;
        commands.push(Command::Slack(Request::Post {
            conversation,
            text,
            id,
        }));
    }

    /// Moves the open Conversation's Read Marker to its newest Message, if that's on screen: the window has focus
    /// and the Timeline is scrolled to the bottom.
    fn mark_seen(&mut self, commands: &mut Vec<Command>) {
        if !self.focused || self.scroll != 0 {
            return;
        }
        let (Some(open), Phase::Ready(workspace)) = (&self.open, &mut self.phase) else {
            return;
        };
        let newest = workspace
            .timelines
            .get(open)
            .and_then(|t| t.messages().next_back())
            .map(|m| m.ts);
        let (Some(newest), Some(activity)) = (newest, workspace.activity.get_mut(open)) else {
            return;
        };
        if newest > activity.read_up_to() {
            activity.mark_read(newest);
            commands.push(Command::Slack(Request::Mark {
                conversation: open.clone(),
                up_to: newest,
            }));
        }
    }

    fn select(&mut self, index: usize) {
        let count = self.workspace().map_or(0, |w| w.conversations.len());
        self.selected = index.min(count.saturating_sub(1));
    }

    fn half_page(&self) -> isize {
        isize::try_from(self.drawn.page / 2)
            .unwrap_or(isize::MAX)
            .max(1)
    }

    /// Scrolls the open Timeline `lines` up (or down, when negative), within what was last drawn.
    fn scroll_by(&mut self, lines: isize, commands: &mut Vec<Command>) {
        self.scroll = self
            .scroll
            .saturating_add_signed(lines)
            .min(self.drawn.max_scroll);
        if self.scroll == self.drawn.max_scroll {
            self.load_older(commands);
        }
    }

    fn open(&mut self, id: ConversationId, commands: &mut Vec<Command>) {
        self.scroll = 0;
        self.drawn.max_scroll = 0;
        self.open = Some(id.clone());
        let Phase::Ready(workspace) = &mut self.phase else {
            return;
        };
        if !workspace.timelines.contains_key(&id) && workspace.loading.insert(id.clone()) {
            commands.push(Command::Slack(Request::History {
                conversation: id,
                before: None,
            }));
        }
    }

    /// Asks for the page before the oldest Message of the open Timeline, if there is one and it isn't on its way.
    fn load_older(&mut self, commands: &mut Vec<Command>) {
        let (Some(open), Phase::Ready(workspace)) = (&self.open, &mut self.phase) else {
            return;
        };
        let Some(timeline) = workspace.timelines.get(open) else {
            return;
        };
        if timeline.has_older() && workspace.loading.insert(open.clone()) {
            commands.push(Command::Slack(Request::History {
                conversation: open.clone(),
                before: timeline.oldest(),
            }));
        }
    }

    fn slack(&mut self, update: Update, commands: &mut Vec<Command>) {
        match update {
            Update::Ready(snapshot) => self.ready(*snapshot, commands),
            Update::SignInFailed(error) => self.phase = Phase::Failed(error.to_string()),
            Update::CustomEmoji(custom) => self.emojis.set_custom(custom),
            Update::History {
                conversation,
                result,
            } => {
                let Phase::Ready(workspace) = &mut self.phase else {
                    return;
                };
                workspace.loading.remove(&conversation);
                match result {
                    Ok(page) => {
                        let people: Vec<UserId> =
                            page.messages.iter().flat_map(people_in).collect();
                        let timeline = workspace.timelines.entry(conversation).or_default();
                        timeline.add_history(page.messages, page.has_more);
                        self.want_users(people, commands);
                    }
                    Err(error) => self.notice = Some(describe(&error)),
                }
            }
            Update::Posted {
                id,
                conversation,
                result,
            } => {
                let Some(at) = self.outbox.iter().position(|outgoing| outgoing.id == id) else {
                    return;
                };
                let outgoing = self.outbox.remove(at);
                match result {
                    Ok(message) => {
                        let Phase::Ready(workspace) = &mut self.phase else {
                            return;
                        };
                        // Slack moves the Read Marker past the user's own Message itself.
                        if let Some(activity) = workspace.activity.get_mut(&conversation) {
                            activity.mark_read(message.ts);
                        }
                        if let Some(timeline) = workspace.timelines.get_mut(&conversation) {
                            timeline.insert(message);
                        }
                    }
                    Err(error) => {
                        self.notice =
                            Some(format!("not sent, back in the draft: {}", describe(&error)));
                        // Put it back where it came from, ahead of anything typed since.
                        let draft = self
                            .drafts
                            .entry(conversation)
                            .or_insert_with(|| draft_of(""));
                        let typed_since = draft.lines().join("\n");
                        let restored = if typed_since.is_empty() {
                            outgoing.text
                        } else {
                            format!("{}\n{typed_since}", outgoing.text)
                        };
                        *draft = draft_of(&restored);
                    }
                }
            }
            Update::User(user) => {
                self.users.insert(user.id, user.name);
            }
            Update::Downloaded { url, result } => {
                if !self.emojis.awaits(&url) {
                    return;
                }
                match result {
                    Ok(bytes) => commands.push(Command::Prepare {
                        url,
                        bytes,
                        fit: Fit::Emoji,
                    }),
                    Err(error) => {
                        tracing::warn!(%error, "couldn't download a custom emoji");
                        self.emojis.failed(url);
                    }
                }
            }
            Update::Live(event) => self.live(event, commands),
            Update::Connection(connection) => self.connection = Some(connection),
        }
    }

    fn ready(&mut self, snapshot: Snapshot, commands: &mut Vec<Command>) {
        let mut conversations: Vec<(Conversation, Activity)> = snapshot.conversations;
        // Channels by name, then DMs with the most recent first, as Slack's sidebar does by default.
        conversations.sort_by(|(a, a_activity), (b, b_activity)| {
            let is_dm = |c: &Conversation| !matches!(c.kind, ConversationKind::Channel { .. });
            is_dm(a).cmp(&is_dm(b)).then_with(|| {
                if is_dm(a) {
                    b_activity.latest().cmp(&a_activity.latest())
                } else {
                    a.name.cmp(&b.name)
                }
            })
        });
        let partners: Vec<UserId> = conversations
            .iter()
            .filter_map(|(c, _)| match &c.kind {
                ConversationKind::Dm { with } => Some(with.clone()),
                _ => None,
            })
            .collect();
        // Put the cursor on the first Conversation with something unread, without opening it: opening marks it
        // read, which the user should choose.
        self.selected = conversations
            .iter()
            .position(|(_, activity)| activity.has_unread())
            .unwrap_or(0);
        let activity = conversations
            .iter()
            .map(|(c, a)| (c.id.clone(), a.clone()))
            .collect();
        self.phase = Phase::Ready(Box::new(Workspace {
            name: snapshot.name,
            me: snapshot.me,
            conversations: conversations.into_iter().map(|(c, _)| c).collect(),
            activity,
            timelines: HashMap::new(),
            loading: HashSet::new(),
        }));
        self.want_users(partners, commands);
    }

    fn live(&mut self, event: LiveEvent, commands: &mut Vec<Command>) {
        let Phase::Ready(workspace) = &mut self.phase else {
            return;
        };
        match event {
            LiveEvent::Posted {
                conversation,
                message,
            } => {
                let people = people_in(&message);
                let is_dm = workspace
                    .conversations
                    .iter()
                    .any(|c| c.id == conversation && matches!(c.kind, ConversationKind::Dm { .. }));
                if message.reply_to.is_none()
                    && !message.is_by(&workspace.me)
                    && let Some(activity) = workspace.activity.get_mut(&conversation)
                {
                    // Every DM Message counts as a Mention, as Slack's badges do.
                    activity.record(message.ts, is_dm || message.mentions(&workspace.me));
                }
                if let Some(timeline) = workspace.timelines.get_mut(&conversation) {
                    timeline.insert(message);
                }
                self.want_users(people, commands);
            }
            LiveEvent::Edited {
                conversation,
                message,
            } => {
                if let Some(timeline) = workspace.timelines.get_mut(&conversation) {
                    timeline.insert(message);
                }
            }
            LiveEvent::Deleted { conversation, ts } => {
                if let Some(timeline) = workspace.timelines.get_mut(&conversation) {
                    timeline.remove(ts);
                }
            }
            LiveEvent::Reacted {
                conversation,
                ts,
                emoji,
                mine,
                change,
            } => {
                if let Some(timeline) = workspace.timelines.get_mut(&conversation) {
                    timeline.react(ts, &emoji, mine, change);
                }
            }
            LiveEvent::Marked {
                conversation,
                up_to,
            } => {
                if let Some(activity) = workspace.activity.get_mut(&conversation) {
                    activity.mark_read(up_to);
                }
            }
        }
    }

    /// Asks for the names of `people` not known or asked for yet.
    fn want_users(
        &mut self,
        people: impl IntoIterator<Item = UserId>,
        commands: &mut Vec<Command>,
    ) {
        for person in people {
            if !self.users.contains_key(&person) && self.requested_users.insert(person.clone()) {
                commands.push(Command::Slack(Request::User(person)));
            }
        }
    }
}

/// What to call `conversation`, given the people known by name: a DM after the other person, anything else by its
/// name.
fn conversation_name<'a>(
    users: &'a HashMap<UserId, String>,
    conversation: &'a Conversation,
) -> &'a str {
    match &conversation.kind {
        ConversationKind::Dm { with } => users.get(with).map_or(with.as_str(), String::as_str),
        ConversationKind::Channel { .. } | ConversationKind::GroupDm => &conversation.name,
    }
}

/// A composer holding `text`, with the cursor at its end.
fn draft_of(text: &str) -> TextArea<'static> {
    let mut draft = TextArea::new(text.lines().map(str::to_owned).collect());
    draft.set_cursor_line_style(Style::default());
    draft.set_placeholder_text("message");
    draft.move_cursor(ratatui_textarea::CursorMove::Bottom);
    draft.move_cursor(ratatui_textarea::CursorMove::End);
    draft
}

/// Everyone a Message names: its author and the people it mentions.
fn people_in(message: &Message) -> Vec<UserId> {
    let author = match &message.author {
        Author::User(id) => Some(id.clone()),
        Author::Bot { .. } => None,
    };
    let mentioned = message.content.inlines().filter_map(|inline| match inline {
        Inline::User(id) => Some(id.clone()),
        _ => None,
    });
    author.into_iter().chain(mentioned).collect()
}

/// A Slack error, as the status bar puts it.
fn describe(error: &Error) -> String {
    match error {
        Error::SignedOut => {
            "signed out: copy a fresh d cookie from the browser into the keyring".to_owned()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use slackers_model::{RichText, Ts, WorkspaceId};
    use slackers_slack::HistoryPage;

    use super::*;

    fn conversation(id: &str, kind: ConversationKind, name: &str) -> Conversation {
        Conversation {
            id: ConversationId::new(id),
            kind,
            name: name.into(),
        }
    }

    fn seconds(s: u64) -> Ts {
        Ts::from_micros(s * 1_000_000)
    }

    fn message(s: u64, author: &str, text: RichText) -> Message {
        Message {
            ts: seconds(s),
            author: Author::User(UserId::new(author)),
            content: text,
            reply_to: None,
            thread: None,
            reactions: Vec::new(),
            files: Vec::new(),
            edited: false,
        }
    }

    fn key(code: KeyCode) -> Event {
        Event::Terminal(TerminalEvent::Key(KeyEvent::from(code)))
    }

    fn keys(app: &mut App, text: &str) -> Vec<Command> {
        text.chars()
            .flat_map(|c| app.update(key(KeyCode::Char(c))))
            .collect()
    }

    fn control(c: char) -> Event {
        Event::Terminal(TerminalEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
    }

    fn c(id: &str) -> ConversationId {
        ConversationId::new(id)
    }

    /// An app signed in to a Workspace with two Channels and a DM, `#random` having unread Messages.
    fn ready_app() -> (App, Vec<Command>) {
        let mut app = App::new("test".into());
        let snapshot = Snapshot {
            workspace: WorkspaceId::new("T1"),
            name: "Test".into(),
            me: UserId::new("UME"),
            conversations: vec![
                (
                    conversation(
                        "D1",
                        ConversationKind::Dm {
                            with: UserId::new("U9"),
                        },
                        "",
                    ),
                    Activity::default(),
                ),
                (
                    conversation("C2", ConversationKind::Channel { private: false }, "random"),
                    Activity::counted(seconds(1), seconds(5), true, 0),
                ),
                (
                    conversation("C1", ConversationKind::Channel { private: true }, "general"),
                    Activity::default(),
                ),
            ],
        };
        let commands = app.update(Event::Slack(Update::Ready(Box::new(snapshot))));
        (app, commands)
    }

    /// `ready_app` with `#random` open and its history loaded.
    fn reading_random() -> App {
        let (mut app, _) = ready_app();
        app.update(key(KeyCode::Enter));
        let page = HistoryPage {
            messages: vec![
                message(5, "U2", RichText::plain("hi")),
                message(2, "U2", RichText::plain("hello")),
            ],
            has_more: true,
        };
        app.update(Event::Slack(Update::History {
            conversation: c("C2"),
            result: Ok(page),
        }));
        app
    }

    fn unread(app: &App, id: &str) -> bool {
        app.workspace().unwrap().activity[&c(id)].has_unread()
    }

    #[test]
    fn signing_in_sorts_the_sidebar_and_selects_the_first_unread_without_opening_it() {
        let (app, commands) = ready_app();
        let ids: Vec<&str> = app
            .workspace()
            .unwrap()
            .conversations
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["C1", "C2", "D1"], "channels by name, then DMs");
        assert_eq!(app.selected, 1);
        assert_eq!(app.open, None);
        assert_eq!(commands, [Command::Slack(Request::User(UserId::new("U9")))]);
    }

    #[test]
    fn opening_loads_history_asks_for_names_and_marks_it_read() {
        let (mut app, _) = ready_app();
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [Command::Slack(Request::History {
                conversation: c("C2"),
                before: None
            })]
        );
        assert!(unread(&app, "C2"));
        let page = HistoryPage {
            messages: vec![message(5, "U2", RichText::plain("hi"))],
            has_more: false,
        };
        let commands = app.update(Event::Slack(Update::History {
            conversation: c("C2"),
            result: Ok(page),
        }));
        assert_eq!(
            commands,
            [
                Command::Slack(Request::User(UserId::new("U2"))),
                Command::Slack(Request::Mark {
                    conversation: c("C2"),
                    up_to: seconds(5)
                }),
            ]
        );
        assert!(!unread(&app, "C2"));
    }

    #[test]
    fn nothing_is_marked_read_while_unfocused_or_scrolled_up() {
        let (mut app, _) = ready_app();
        app.update(Event::Terminal(TerminalEvent::FocusLost));
        app.update(key(KeyCode::Enter));
        let page = HistoryPage {
            messages: vec![message(5, "U2", RichText::plain("hi"))],
            has_more: true,
        };
        app.update(Event::Slack(Update::History {
            conversation: c("C2"),
            result: Ok(page),
        }));
        assert!(unread(&app, "C2"));

        app.update(Event::Drawn(Drawn {
            max_scroll: 5,
            page: 10,
            ..Drawn::default()
        }));
        app.update(key(KeyCode::Char('k')));
        assert!(
            app.update(Event::Terminal(TerminalEvent::FocusGained))
                .is_empty(),
            "scrolled up"
        );
        assert!(unread(&app, "C2"));

        let commands = app.update(key(KeyCode::Char('G')));
        assert_eq!(
            commands,
            [Command::Slack(Request::Mark {
                conversation: c("C2"),
                up_to: seconds(5)
            })]
        );
    }

    #[test]
    fn scrolling_to_the_top_loads_older_history_once() {
        let mut app = reading_random();
        app.update(Event::Drawn(Drawn {
            max_scroll: 5,
            page: 10,
            ..Drawn::default()
        }));
        assert!(app.update(key(KeyCode::Char('k'))).is_empty());
        let commands = app.update(key(KeyCode::Char('g')));
        assert_eq!(app.scroll, 5);
        assert_eq!(
            commands,
            [Command::Slack(Request::History {
                conversation: c("C2"),
                before: Some(seconds(2))
            })]
        );
        assert!(
            app.update(key(KeyCode::Char('k'))).is_empty(),
            "already on its way"
        );
    }

    #[test]
    fn live_messages_raise_activity_but_not_my_own_or_replies() {
        let (mut app, _) = ready_app();
        let me = UserId::new("UME");
        let mut mention = RichText::plain("hey ");
        if let slackers_model::Block::Paragraph(inlines) = &mut mention.blocks[0] {
            inlines.push(Inline::User(me.clone()));
        }
        let post = |conversation: &str, message: Message| {
            Event::Slack(Update::Live(LiveEvent::Posted {
                conversation: c(conversation),
                message,
            }))
        };
        app.update(post("C1", message(10, "UME", RichText::plain("mine"))));
        let mut reply = message(11, "U2", RichText::plain("reply"));
        reply.reply_to = Some(seconds(1));
        app.update(post("C1", reply));
        assert!(!unread(&app, "C1"));

        app.update(post("C1", message(12, "U2", mention)));
        app.update(post("D1", message(13, "U9", RichText::plain("dm"))));
        let mentions = |app: &App, id: &str| app.workspace().unwrap().activity[&c(id)].mentions();
        assert_eq!(mentions(&app, "C1"), 1);
        assert_eq!(mentions(&app, "D1"), 1, "DMs count as mentions");

        let marked = LiveEvent::Marked {
            conversation: c("C1"),
            up_to: seconds(12),
        };
        app.update(Event::Slack(Update::Live(marked)));
        assert!(!unread(&app, "C1"));
    }

    #[test]
    fn insert_mode_writes_and_enter_sends() {
        let mut app = reading_random();
        assert!(keys(&mut app, "i").is_empty());
        assert_eq!(app.mode, Mode::Insert);
        keys(&mut app, "hi :wave:");
        let newline = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        app.update(Event::Terminal(TerminalEvent::Key(newline)));
        keys(&mut app, "there");

        let commands = app.update(key(KeyCode::Enter));
        let text = "hi :wave:\nthere".to_owned();
        assert_eq!(
            commands,
            [Command::Slack(Request::Post {
                conversation: c("C2"),
                text,
                id: 0
            })]
        );
        assert_eq!(app.outbox.len(), 1);
        assert!(app.draft().unwrap().is_empty());
        assert!(
            app.update(key(KeyCode::Enter)).is_empty(),
            "nothing to send"
        );

        app.update(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn a_posted_message_joins_the_timeline_and_a_failed_one_returns_to_the_draft() {
        let mut app = reading_random();
        keys(&mut app, "ifirst");
        app.update(key(KeyCode::Enter));
        keys(&mut app, "second");
        app.update(key(KeyCode::Enter));

        let posted = message(9, "UME", RichText::plain("first"));
        app.update(Event::Slack(Update::Posted {
            id: 0,
            conversation: c("C2"),
            result: Ok(posted),
        }));
        let timeline = &app.workspace().unwrap().timelines[&c("C2")];
        assert_eq!(timeline.messages().next_back().unwrap().ts, seconds(9));

        keys(&mut app, "third");
        app.update(Event::Slack(Update::Posted {
            id: 1,
            conversation: c("C2"),
            result: Err(Error::SignedOut),
        }));
        assert!(app.outbox.is_empty());
        assert_eq!(app.draft().unwrap().lines(), ["second", "third"]);
        assert!(app.notice.as_deref().unwrap().starts_with("not sent"));
    }

    #[test]
    fn ctrl_x_ctrl_e_hands_the_draft_to_the_editor() {
        let mut app = reading_random();
        keys(&mut app, "idraft");
        assert!(app.update(control('x')).is_empty());
        assert_eq!(
            app.update(control('e')),
            [Command::Edit {
                conversation: c("C2"),
                text: "draft".into()
            }]
        );
        app.update(Event::Edited {
            conversation: c("C2"),
            outcome: Ok("edited\ntext".into()),
        });
        assert_eq!(app.draft().unwrap().lines(), ["edited", "text"]);
    }

    #[test]
    fn only_colon_q_quits() {
        let (mut app, _) = ready_app();
        assert!(keys(&mut app, "q").is_empty());
        assert!(app.update(control('c')).is_empty());
        assert_eq!(
            app.notice.as_deref(),
            Some("type :q and press Enter to quit")
        );

        keys(&mut app, ":wq");
        assert_eq!(app.mode, Mode::Command("wq".into()));
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.notice.as_deref(), Some("not a command: :wq"));

        keys(&mut app, ":q");
        assert_eq!(app.update(key(KeyCode::Enter)), [Command::Quit]);
    }

    fn hit_names(app: &App) -> Vec<String> {
        let Mode::Search(search) = &app.mode else {
            panic!("not searching: {:?}", app.mode)
        };
        let workspace = app.workspace().unwrap();
        search
            .hits
            .iter()
            .map(|hit| {
                app.conversation_name(&workspace.conversations[hit.index])
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn slash_narrows_the_sidebar_and_enter_opens_the_pick() {
        let (mut app, _) = ready_app();
        app.focus = Focus::Timeline;
        keys(&mut app, "/");
        assert_eq!(app.focus, Focus::Sidebar);
        assert_eq!(
            hit_names(&app),
            ["general", "random", "U9"],
            "everything, before typing"
        );

        app.update(Event::Slack(Update::User(slackers_slack::User {
            id: UserId::new("U9"),
            name: "Rand Al'Thor".into(),
        })));
        keys(&mut app, "ran");
        assert_eq!(
            hit_names(&app),
            ["random", "Rand Al'Thor"],
            "DMs match by the person's name"
        );

        app.update(control('n'));
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.open, Some(c("D1")));
        assert_eq!(app.selected, 2);
        assert_eq!(app.focus, Focus::Timeline);
        assert_eq!(
            commands,
            [Command::Slack(Request::History {
                conversation: c("D1"),
                before: None
            })]
        );
    }

    #[test]
    fn escape_and_backspacing_past_the_slash_cancel_a_search() {
        let (mut app, _) = ready_app();
        keys(&mut app, "/gen");
        app.update(key(KeyCode::Esc));
        assert_eq!(
            (&app.mode, app.selected, &app.open),
            (&Mode::Normal, 1, &None)
        );

        keys(&mut app, "/g");
        app.update(key(KeyCode::Backspace));
        assert_eq!(hit_names(&app).len(), 3);
        app.update(key(KeyCode::Backspace));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn a_search_without_hits_says_so() {
        let (mut app, _) = ready_app();
        keys(&mut app, "/zzz");
        assert!(hit_names(&app).is_empty());
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.notice.as_deref(), Some("no conversation matches /zzz"));
    }

    #[test]
    fn i_needs_an_open_conversation() {
        let (mut app, _) = ready_app();
        keys(&mut app, "i");
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.notice.as_deref(), Some("open a conversation first"));
    }

    #[test]
    fn wanted_pictures_are_downloaded_once_then_prepared() {
        let (mut app, _) = ready_app();
        let mut custom = slackers_model::CustomEmojiSet::default();
        custom.insert_image("parrot", "https://e/p.gif");
        app.update(Event::Slack(Update::CustomEmoji(custom)));

        let wanted = || {
            Event::Drawn(Drawn {
                wanted_pictures: vec!["https://e/p.gif".into()],
                ..Drawn::default()
            })
        };
        assert_eq!(
            app.update(wanted()),
            [Command::Slack(Request::Download {
                url: "https://e/p.gif".into()
            })]
        );
        assert!(app.update(wanted()).is_empty());
        let downloaded = Update::Downloaded {
            url: "https://e/p.gif".into(),
            result: Ok(vec![1, 2]),
        };
        assert_eq!(
            app.update(Event::Slack(downloaded)),
            [Command::Prepare {
                url: "https://e/p.gif".into(),
                bytes: vec![1, 2],
                fit: Fit::Emoji
            }]
        );
    }
}
