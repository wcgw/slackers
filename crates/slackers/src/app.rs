//! The app's state, and how events change it. No I/O here: every effect is a [`Command`] for the runtime to carry
//! out, so that `update` can be tested on its own.
//!
//! Keys work as in vim: Normal mode moves around (`j`/`k` from Message to Message, `Enter` opens a Thread, `\`
//! jumps to the sidebar), `i` enters Insert mode to write in the open Conversation or Thread, `/` searches the
//! sidebar, and `:` enters Command mode (`:q` quits).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::TextArea;
use slackers_gfx::{Fit, Picture};
use slackers_model::{
    Activity, Author, Conversation, ConversationId, ConversationKind, Inline, Message, Timeline,
    Ts, UserId,
};
use slackers_slack::{Connection, Error, LiveEvent, Request, Snapshot, Update, User};

use crate::emoji::Emojis;
use crate::pane::{Pane, Settled};
use crate::pictures::Pictures;
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
        place: Place,
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
    /// When something drawn next changes frame, as animation time; `None` when nothing animates.
    pub(crate) next_change: Option<Duration>,
    /// Where the open Conversation's pane settled; `None` when it wasn't on screen.
    pub(crate) conversation: Option<Settled>,
    /// Where the open Thread's pane settled; `None` when it wasn't on screen.
    pub(crate) thread: Option<Settled>,
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
    /// Hand the terminal to `$EDITOR` to edit `text`, the draft for `place`, then report back with
    /// [`Event::Edited`] and [`Event::PicturesLost`].
    Edit {
        place: Place,
        text: String,
    },
    Quit,
}

/// What keys do, as in vim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Keys move around the pane in [`Focus`].
    Normal,
    /// Keys write the draft for the focused pane's [`Place`].
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
    /// The open Conversation's Messages.
    Timeline,
    /// The open Thread's Messages.
    Thread,
}

/// Where a draft is written and a Message posted: a Conversation, or a Thread in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Place {
    pub(crate) conversation: ConversationId,
    /// The Thread, by its parent; `None` for the Conversation itself.
    pub(crate) thread: Option<Ts>,
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
    /// Conversations' top-level Messages.
    pub(crate) timelines: HashMap<ConversationId, Timeline>,
    /// Threads, by their Conversation and parent.
    pub(crate) threads: HashMap<(ConversationId, Ts), Timeline>,
    /// Conversations with a history request in flight.
    loading: HashSet<ConversationId>,
    /// Threads with a request for their Replies in flight.
    loading_threads: HashSet<(ConversationId, Ts)>,
}

/// A Message on its way to Slack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outgoing {
    /// Matches the [`Request::Post`] to its [`Update::Posted`].
    id: u64,
    pub(crate) place: Place,
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
    /// People, as far as they've been looked up.
    pub(crate) users: HashMap<UserId, User>,
    requested_users: HashSet<UserId>,
    pub(crate) emojis: Emojis,
    /// Custom Emoji and avatars, fetched as they come on screen.
    pub(crate) pictures: Pictures,
    pub(crate) mode: Mode,
    /// The sidebar entry under the cursor.
    pub(crate) selected: usize,
    pub(crate) open: Option<ConversationId>,
    /// Where the user is in the open Conversation.
    pub(crate) conversation_pane: Pane,
    /// The open Thread, in the open Conversation, by its parent.
    pub(crate) open_thread: Option<Ts>,
    /// Where the user is in the open Thread.
    pub(crate) thread_pane: Pane,
    pub(crate) focus: Focus,
    /// Whether the sidebar shows. `\` hides it from within it, and brings it back. Whenever focus is on it, it shows.
    pub(crate) sidebar_shown: bool,
    /// Unsent text, per Place.
    pub(crate) drafts: HashMap<Place, TextArea<'static>>,
    /// Messages posted but not yet confirmed, oldest first.
    pub(crate) outbox: Vec<Outgoing>,
    next_post: u64,
    /// Whether `Ctrl-x` was just pressed in Insert mode, starting `Ctrl-x Ctrl-e`.
    awaiting_chord: bool,
    /// Whether the terminal window has focus. Messages count as read only while it does.
    focused: bool,
    /// Whether the open Conversation's pane was on screen at the last draw; a Thread may cover it.
    conversation_shown: bool,
    finder: Finder,
    /// When something drawn next changes frame, as animation time.
    next_change: Option<Duration>,
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
            pictures: Pictures::default(),
            mode: Mode::Normal,
            selected: 0,
            open: None,
            conversation_pane: Pane::default(),
            open_thread: None,
            thread_pane: Pane::default(),
            focus: Focus::Sidebar,
            sidebar_shown: true,
            drafts: HashMap::new(),
            outbox: Vec::new(),
            next_post: 0,
            awaiting_chord: false,
            focused: true,
            conversation_shown: false,
            finder: Finder::new(),
            next_change: None,
            epoch: Instant::now(),
        }
    }

    /// Time since animations started.
    pub(crate) fn animation_time(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// When the screen next needs redrawing for animation alone, as animation time.
    pub(crate) fn next_change(&self) -> Option<Duration> {
        self.next_change
    }

    pub(crate) fn workspace(&self) -> Option<&Workspace> {
        match &self.phase {
            Phase::Ready(workspace) => Some(workspace),
            Phase::SigningIn | Phase::Failed(_) => None,
        }
    }

    /// What to call `user`: their name once looked up, else their id.
    pub(crate) fn user_name<'a>(&'a self, user: &'a UserId) -> &'a str {
        self.users
            .get(user)
            .map_or(user.as_str(), |user| user.name.as_str())
    }

    /// Where `author`'s avatar image is, as far as known.
    pub(crate) fn avatar<'a>(&'a self, author: &'a Author) -> Option<&'a str> {
        match author {
            Author::User(id) => self.users.get(id)?.avatar.as_deref(),
            Author::Bot { avatar, .. } => avatar.as_deref(),
        }
    }

    /// What to call `conversation`: a DM after the other person, anything else by its name.
    pub(crate) fn conversation_name<'a>(&'a self, conversation: &'a Conversation) -> &'a str {
        conversation_name(&self.users, conversation)
    }

    /// Where Insert mode writes: the open Thread when its pane has focus, else the open Conversation.
    pub(crate) fn writing_place(&self) -> Option<Place> {
        let thread = self.open_thread.filter(|_| self.focus == Focus::Thread);
        Some(Place {
            conversation: self.open.clone()?,
            thread,
        })
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
            Event::PictureReady { url, picture } => self.pictures.loaded(url, picture),
            Event::PictureFailed { url } => self.pictures.failed(url),
            Event::PicturesLost => self.pictures.forget_all(),
            Event::Edited { place, outcome } => match outcome {
                Ok(text) => {
                    let draft = draft_of(&text, &place);
                    self.drafts.insert(place, draft);
                }
                Err(reason) => self.notice = Some(reason),
            },
            Event::Drawn(drawn) => {
                for url in &drawn.wanted_pictures {
                    if self.pictures.request(url) {
                        commands.push(Command::Slack(Request::Download { url: url.clone() }));
                    }
                }
                self.next_change = drawn.next_change;
                self.conversation_shown = drawn.conversation.is_some();
                if let Some(settled) = drawn.conversation {
                    self.conversation_pane.adopt(settled);
                    if settled.scroll == settled.max_scroll {
                        self.load_older(&mut commands);
                    }
                }
                if let Some(settled) = drawn.thread {
                    self.thread_pane.adopt(settled);
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
            (KeyCode::Char('\\'), false) => {
                match self.focus {
                    Focus::Sidebar if self.open.is_some() => {
                        self.sidebar_shown = false;
                        self.focus = Focus::Timeline;
                    }
                    Focus::Sidebar => self.notice = Some("open a conversation first".to_owned()),
                    Focus::Timeline | Focus::Thread => self.focus_sidebar(),
                }
                return;
            }
            (KeyCode::Char('/'), false) => {
                self.mode = Mode::Search(Search {
                    query: String::new(),
                    hits: Vec::new(),
                    selected: 0,
                });
                self.focus_sidebar();
                return;
            }
            (KeyCode::Char('i'), false) => {
                if self.focus == Focus::Sidebar {
                    self.focus = Focus::Timeline;
                }
                match self.writing_place() {
                    Some(place) => {
                        self.drafts
                            .entry(place.clone())
                            .or_insert_with(|| draft_of("", &place));
                        self.mode = Mode::Insert;
                    }
                    None => self.notice = Some("open a conversation first".to_owned()),
                }
                return;
            }
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Timeline,
                    Focus::Timeline if self.open_thread.is_some() => Focus::Thread,
                    Focus::Timeline | Focus::Thread if self.sidebar_shown => Focus::Sidebar,
                    Focus::Timeline | Focus::Thread => Focus::Timeline,
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
            Focus::Timeline => match key.code {
                KeyCode::Enter => self.open_selected_thread(commands),
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => self.focus_sidebar(),
                _ => {
                    let (Some(open), Phase::Ready(workspace)) = (&self.open, &self.phase) else {
                        return;
                    };
                    if let Some(timeline) = workspace.timelines.get(open) {
                        move_in(&mut self.conversation_pane, timeline, key);
                    }
                }
            },
            Focus::Thread => match key.code {
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => {
                    self.open_thread = None;
                    self.focus = Focus::Timeline;
                }
                _ => {
                    let (Some(open), Some(parent), Phase::Ready(workspace)) =
                        (&self.open, self.open_thread, &self.phase)
                    else {
                        return;
                    };
                    if let Some(thread) = workspace.threads.get(&(open.clone(), parent)) {
                        move_in(&mut self.thread_pane, thread, key);
                    }
                }
            },
        }
    }

    fn insert_key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let Some(place) = self.writing_place() else {
            self.mode = Mode::Normal;
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let chord = std::mem::take(&mut self.awaiting_chord);
        let draft = |drafts: &mut HashMap<Place, TextArea<'static>>, place: &Place| {
            drafts
                .entry(place.clone())
                .or_insert_with(|| draft_of("", place))
                .lines()
                .join("\n")
        };
        match (key.code, control) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => self.mode = Mode::Normal,
            (KeyCode::Char('x'), true) => self.awaiting_chord = true,
            (KeyCode::Char('e'), true) if chord => {
                let text = draft(&mut self.drafts, &place);
                commands.push(Command::Edit { place, text });
            }
            // Shift+Enter needs the kitty keyboard protocol to tell it from Enter; Alt+Enter works without.
            (KeyCode::Enter, _)
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.drafts
                    .entry(place.clone())
                    .or_insert_with(|| draft_of("", &place))
                    .insert_newline();
            }
            (KeyCode::Enter, _) => self.send(place, commands),
            _ => {
                self.drafts
                    .entry(place.clone())
                    .or_insert_with(|| draft_of("", &place))
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

    /// Opens the Thread of the open Conversation's selected Message, in its own pane, and asks for its Replies.
    /// Any Message can start a Thread: one without Replies yet opens empty, ready for one.
    fn open_selected_thread(&mut self, commands: &mut Vec<Command>) {
        let (Some(open), Phase::Ready(workspace)) = (&self.open, &mut self.phase) else {
            return;
        };
        let selected = workspace
            .timelines
            .get(open)
            .and_then(|t| self.conversation_pane.selected(t));
        let Some(parent) = selected else { return };
        self.open_thread = Some(parent);
        self.thread_pane = Pane::default();
        self.focus = Focus::Thread;
        let key = (open.clone(), parent);
        if !workspace.threads.contains_key(&key) && workspace.loading_threads.insert(key) {
            commands.push(Command::Slack(Request::Replies {
                conversation: open.clone(),
                parent,
            }));
        }
    }

    /// Posts the draft for `place`, unless it's blank, and empties it.
    fn send(&mut self, place: Place, commands: &mut Vec<Command>) {
        let Some(draft) = self.drafts.get_mut(&place) else {
            return;
        };
        let text = draft.lines().join("\n");
        if text.trim().is_empty() {
            return;
        }
        *draft = draft_of("", &place);
        let id = self.next_post;
        self.next_post += 1;
        match place.thread {
            Some(_) => self.thread_pane.newest(),
            None => self.conversation_pane.newest(),
        }
        commands.push(Command::Slack(Request::Post {
            conversation: place.conversation.clone(),
            thread: place.thread,
            text: text.clone(),
            id,
        }));
        self.outbox.push(Outgoing { id, place, text });
    }

    /// Moves the open Conversation's Read Marker to its newest Message, if that's on screen: the window has focus,
    /// the Conversation's pane shows, and the newest Message is selected at the bottom.
    fn mark_seen(&mut self, commands: &mut Vec<Command>) {
        if !self.focused || !self.conversation_shown || !self.conversation_pane.at_newest() {
            return;
        }
        let (Some(open), Phase::Ready(workspace)) = (&self.open, &mut self.phase) else {
            return;
        };
        let newest = workspace.timelines.get(open).and_then(Timeline::newest);
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

    /// Moves focus to the sidebar, showing it if it's hidden.
    fn focus_sidebar(&mut self) {
        self.sidebar_shown = true;
        self.focus = Focus::Sidebar;
    }

    fn select(&mut self, index: usize) {
        let count = self.workspace().map_or(0, |w| w.conversations.len());
        self.selected = index.min(count.saturating_sub(1));
    }

    fn open(&mut self, id: ConversationId, commands: &mut Vec<Command>) {
        self.conversation_pane = Pane::default();
        self.conversation_shown = false;
        self.open_thread = None;
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
            Update::Replies {
                conversation,
                parent,
                result,
            } => {
                let Phase::Ready(workspace) = &mut self.phase else {
                    return;
                };
                let key = (conversation, parent);
                workspace.loading_threads.remove(&key);
                match result {
                    Ok(messages) => {
                        let people: Vec<UserId> = messages.iter().flat_map(people_in).collect();
                        let thread = workspace
                            .threads
                            .entry(key)
                            .or_insert_with(|| Timeline::thread(parent));
                        thread.add_history(messages, false);
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
                        if message.reply_to.is_none()
                            && let Some(activity) = workspace.activity.get_mut(&conversation)
                        {
                            activity.mark_read(message.ts);
                        }
                        workspace.add(&conversation, message);
                    }
                    Err(error) => {
                        self.notice =
                            Some(format!("not sent, back in the draft: {}", describe(&error)));
                        // Put it back where it came from, ahead of anything typed since.
                        let place = outgoing.place;
                        let draft = self
                            .drafts
                            .entry(place.clone())
                            .or_insert_with(|| draft_of("", &place));
                        let typed_since = draft.lines().join("\n");
                        let restored = if typed_since.is_empty() {
                            outgoing.text
                        } else {
                            format!("{}\n{typed_since}", outgoing.text)
                        };
                        *draft = draft_of(&restored, &place);
                    }
                }
            }
            Update::User(user) => {
                self.users.insert(user.id.clone(), user);
            }
            Update::Downloaded { url, result } => {
                if !self.pictures.awaits(&url) {
                    return;
                }
                match result {
                    Ok(bytes) => commands.push(Command::Prepare {
                        url,
                        bytes,
                        fit: Fit::Emoji,
                    }),
                    Err(error) => {
                        tracing::warn!(%error, "couldn't download a picture");
                        self.pictures.failed(url);
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
            threads: HashMap::new(),
            loading: HashSet::new(),
            loading_threads: HashSet::new(),
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
                workspace.add(&conversation, message);
                self.want_users(people, commands);
            }
            LiveEvent::Edited {
                conversation,
                message,
            } => {
                if let Some(timeline) = workspace.timelines.get_mut(&conversation) {
                    timeline.replace(message.clone());
                }
                let thread = (conversation, message.reply_to.unwrap_or(message.ts));
                if let Some(thread) = workspace.threads.get_mut(&thread) {
                    thread.replace(message);
                }
            }
            LiveEvent::Deleted { conversation, ts } => {
                for timeline in workspace.timelines_of(&conversation) {
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
                for timeline in workspace.timelines_of(&conversation) {
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

impl Workspace {
    /// Adds a Message posted in `conversation` wherever it's held: its Timeline, which counts a Reply on its parent,
    /// and its Thread's.
    fn add(&mut self, conversation: &ConversationId, message: Message) {
        if let Some(parent) = message.reply_to
            && let Some(thread) = self.threads.get_mut(&(conversation.clone(), parent))
        {
            thread.insert(message.clone());
        }
        if let Some(timeline) = self.timelines.get_mut(conversation) {
            timeline.insert(message);
        }
    }

    /// `conversation`'s Timeline and those of its Threads, as far as they're held.
    fn timelines_of<'a>(
        &'a mut self,
        conversation: &'a ConversationId,
    ) -> impl Iterator<Item = &'a mut Timeline> {
        let threads = self
            .threads
            .iter_mut()
            .filter(move |((c, _), _)| c == conversation)
            .map(|(_, t)| t);
        self.timelines
            .get_mut(conversation)
            .into_iter()
            .chain(threads)
    }
}

/// Applies a key that moves around a pane of Messages to `pane`, over `timeline`: `j`/`k` from Message to Message,
/// `g`/`G` to the oldest and newest, `Ctrl-e`/`Ctrl-y` a line, `Ctrl-d`/`Ctrl-u` half a page. Other keys do nothing.
fn move_in(pane: &mut Pane, timeline: &Timeline, key: KeyEvent) {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, control) {
        (KeyCode::Char('e'), true) => pane.scroll_by(-1),
        (KeyCode::Char('y'), true) => pane.scroll_by(1),
        (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => pane.scroll_by(-pane.half_page()),
        (KeyCode::Char('u'), true) | (KeyCode::PageUp, _) => pane.scroll_by(pane.half_page()),
        (KeyCode::Char('j') | KeyCode::Down, false) => pane.down(timeline),
        (KeyCode::Char('k') | KeyCode::Up, false) => pane.up(timeline),
        (KeyCode::Char('g') | KeyCode::Home, false) => pane.oldest(timeline),
        (KeyCode::Char('G') | KeyCode::End, false) => pane.newest(),
        _ => {}
    }
}

/// What to call `conversation`, given the people known: a DM after the other person, anything else by its name.
fn conversation_name<'a>(
    users: &'a HashMap<UserId, User>,
    conversation: &'a Conversation,
) -> &'a str {
    match &conversation.kind {
        ConversationKind::Dm { with } => users
            .get(with)
            .map_or(with.as_str(), |user| user.name.as_str()),
        ConversationKind::Channel { .. } | ConversationKind::GroupDm => &conversation.name,
    }
}

/// A composer for `place` holding `text`, with the cursor at its end.
fn draft_of(text: &str, place: &Place) -> TextArea<'static> {
    let mut draft = TextArea::new(text.lines().map(str::to_owned).collect());
    draft.set_cursor_line_style(Style::default());
    draft.set_placeholder_text(if place.thread.is_some() {
        "reply"
    } else {
        "message"
    });
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
    use slackers_model::{RichText, WorkspaceId};
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

    fn reply(s: u64, parent: u64) -> Message {
        Message {
            reply_to: Some(seconds(parent)),
            ..message(s, "U3", RichText::plain("reply"))
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

    fn place(conversation: &str, thread: Option<u64>) -> Place {
        Place {
            conversation: c(conversation),
            thread: thread.map(seconds),
        }
    }

    /// Reports a draw that showed the Conversation's pane, as tall as `height`, with everything fitting: the pane
    /// adopts where it settled.
    fn drawn(app: &mut App, height: usize) -> Vec<Command> {
        let settled = Settled {
            cursor: app.conversation_pane.cursor(),
            height,
            ..Settled::default()
        };
        app.update(Event::Drawn(Drawn {
            conversation: Some(settled),
            ..Drawn::default()
        }))
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

    /// `ready_app` with `#random` open, drawn, and holding Messages at 2 and 5 seconds, the latter with a Reply.
    fn reading_random() -> App {
        let (mut app, _) = ready_app();
        app.update(key(KeyCode::Enter));
        let parent = Message {
            thread: Some(slackers_model::ThreadSummary {
                replies: 1,
                latest: seconds(6),
            }),
            ..message(5, "U2", RichText::plain("hi"))
        };
        let page = HistoryPage {
            messages: vec![parent, message(2, "U2", RichText::plain("hello"))],
            has_more: true,
        };
        app.update(Event::Slack(Update::History {
            conversation: c("C2"),
            result: Ok(page),
        }));
        drawn(&mut app, 10);
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
    fn opening_loads_history_asks_for_names_and_marks_it_read_once_shown() {
        let (mut app, _) = ready_app();
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [Command::Slack(Request::History {
                conversation: c("C2"),
                before: None
            })]
        );
        let page = HistoryPage {
            messages: vec![message(5, "U2", RichText::plain("hi"))],
            has_more: false,
        };
        let commands = app.update(Event::Slack(Update::History {
            conversation: c("C2"),
            result: Ok(page),
        }));
        assert_eq!(commands, [Command::Slack(Request::User(UserId::new("U2")))]);
        assert!(unread(&app, "C2"), "not on screen yet");

        assert_eq!(
            drawn(&mut app, 10),
            [Command::Slack(Request::Mark {
                conversation: c("C2"),
                up_to: seconds(5)
            })]
        );
        assert!(!unread(&app, "C2"));
    }

    #[test]
    fn nothing_is_marked_read_while_unfocused_or_away_from_the_newest() {
        let (mut app, _) = ready_app();
        app.update(Event::Terminal(TerminalEvent::FocusLost));
        app.update(key(KeyCode::Enter));
        let page = HistoryPage {
            messages: vec![
                message(5, "U2", RichText::plain("hi")),
                message(2, "U2", RichText::plain("ho")),
            ],
            has_more: false,
        };
        app.update(Event::Slack(Update::History {
            conversation: c("C2"),
            result: Ok(page),
        }));
        drawn(&mut app, 10);
        assert!(unread(&app, "C2"));

        app.update(key(KeyCode::Char('k')));
        drawn(&mut app, 10);
        assert!(
            app.update(Event::Terminal(TerminalEvent::FocusGained))
                .is_empty(),
            "an older one is selected"
        );
        assert!(unread(&app, "C2"));

        assert_eq!(
            app.update(key(KeyCode::Char('G'))),
            [Command::Slack(Request::Mark {
                conversation: c("C2"),
                up_to: seconds(5)
            })]
        );
    }

    #[test]
    fn reaching_the_top_loads_older_history_once() {
        let (mut app, _) = ready_app();
        app.update(key(KeyCode::Enter));
        let page = HistoryPage {
            messages: vec![
                message(5, "U2", RichText::plain("hi")),
                message(2, "U2", RichText::plain("ho")),
            ],
            has_more: true,
        };
        app.update(Event::Slack(Update::History {
            conversation: c("C2"),
            result: Ok(page),
        }));
        let drawn = |settled| {
            Event::Drawn(Drawn {
                conversation: Some(settled),
                ..Drawn::default()
            })
        };
        let midway = Settled {
            scroll: 2,
            max_scroll: 5,
            height: 10,
            ..Settled::default()
        };
        assert!(app.update(drawn(midway)).is_empty());

        let at_top = Settled {
            scroll: 5,
            max_scroll: 5,
            height: 10,
            ..Settled::default()
        };
        let commands = app.update(drawn(at_top));
        assert_eq!(
            commands,
            [Command::Slack(Request::History {
                conversation: c("C2"),
                before: Some(seconds(2))
            })]
        );
        let again = app.update(Event::Drawn(Drawn {
            conversation: Some(at_top),
            ..Drawn::default()
        }));
        assert!(again.is_empty(), "already on its way");
    }

    #[test]
    fn j_and_k_move_from_message_to_message() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        let selected = |app: &App| {
            let timeline = &app.workspace().unwrap().timelines[&c("C2")];
            app.conversation_pane.selected(timeline)
        };
        assert_eq!(selected(&app), Some(seconds(5)));
        keys(&mut app, "k");
        assert_eq!(selected(&app), Some(seconds(2)));
        keys(&mut app, "j");
        assert_eq!(selected(&app), Some(seconds(5)));
    }

    #[test]
    fn enter_opens_the_selected_messages_thread_and_esc_closes_it() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            (app.open_thread, app.focus),
            (Some(seconds(5)), Focus::Thread)
        );
        assert_eq!(
            commands,
            [Command::Slack(Request::Replies {
                conversation: c("C2"),
                parent: seconds(5)
            })]
        );

        let replies = vec![message(5, "U2", RichText::plain("hi")), reply(6, 5)];
        let result = Ok(replies);
        let commands = app.update(Event::Slack(Update::Replies {
            conversation: c("C2"),
            parent: seconds(5),
            result,
        }));
        assert_eq!(commands, [Command::Slack(Request::User(UserId::new("U3")))]);
        let thread = &app.workspace().unwrap().threads[&(c("C2"), seconds(5))];
        assert_eq!(thread.messages().len(), 2);

        app.update(key(KeyCode::Esc));
        assert_eq!((app.open_thread, app.focus), (None, Focus::Timeline));
        app.update(key(KeyCode::Enter));
        assert_eq!(
            app.open_thread,
            Some(seconds(5)),
            "opens again without asking twice"
        );
    }

    #[test]
    fn backslash_jumps_to_the_sidebar_from_either_pane() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        keys(&mut app, "\\");
        assert_eq!(app.focus, Focus::Sidebar);

        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Thread);
        keys(&mut app, "\\");
        assert_eq!(
            (app.focus, app.open_thread),
            (Focus::Sidebar, Some(seconds(5))),
            "the thread stays open"
        );
    }

    #[test]
    fn backslash_in_the_sidebar_hides_it_and_brings_it_back() {
        let mut app = reading_random();
        app.focus = Focus::Sidebar;
        keys(&mut app, "\\");
        assert_eq!((app.sidebar_shown, app.focus), (false, Focus::Timeline));

        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Timeline, "tab skips the hidden sidebar");
        keys(&mut app, "\\");
        assert_eq!((app.sidebar_shown, app.focus), (true, Focus::Sidebar));

        keys(&mut app, "\\");
        keys(&mut app, "/");
        assert_eq!(
            (app.sidebar_shown, app.focus),
            (true, Focus::Sidebar),
            "searching shows it"
        );
        app.update(key(KeyCode::Esc));
        keys(&mut app, "\\");
        app.update(key(KeyCode::Esc));
        assert_eq!(
            (app.sidebar_shown, app.focus),
            (true, Focus::Sidebar),
            "so does esc from the messages"
        );
    }

    #[test]
    fn the_sidebar_stays_while_nothing_is_open() {
        let (mut app, _) = ready_app();
        keys(&mut app, "\\");
        assert_eq!((app.sidebar_shown, app.focus), (true, Focus::Sidebar));
        assert_eq!(app.notice.as_deref(), Some("open a conversation first"));
    }

    #[test]
    fn tab_visits_the_thread_pane_only_while_one_is_open() {
        let mut app = reading_random();
        app.focus = Focus::Sidebar;
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Sidebar);
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Sidebar);
        app.update(key(KeyCode::Tab));
        app.update(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Thread);
    }

    #[test]
    fn a_reply_written_in_the_thread_pane_posts_there_and_lands_in_both_panes() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        app.update(key(KeyCode::Enter));
        let result = Ok(vec![message(5, "U2", RichText::plain("hi"))]);
        app.update(Event::Slack(Update::Replies {
            conversation: c("C2"),
            parent: seconds(5),
            result,
        }));

        keys(&mut app, "ion it");
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            commands,
            [Command::Slack(Request::Post {
                conversation: c("C2"),
                thread: Some(seconds(5)),
                text: "on it".into(),
                id: 0,
            })]
        );
        assert_eq!(app.outbox[0].place, place("C2", Some(5)));

        let posted = Message {
            reply_to: Some(seconds(5)),
            ..message(7, "UME", RichText::plain("on it"))
        };
        app.update(Event::Slack(Update::Posted {
            id: 0,
            conversation: c("C2"),
            result: Ok(posted.clone()),
        }));
        // The live echo of the same Reply changes nothing.
        let echo = Update::Live(LiveEvent::Posted {
            conversation: c("C2"),
            message: posted,
        });
        app.update(Event::Slack(echo));

        let workspace = app.workspace().unwrap();
        assert_eq!(
            workspace.threads[&(c("C2"), seconds(5))].messages().len(),
            2
        );
        let parent = workspace.timelines[&c("C2")].get(seconds(5)).unwrap();
        assert_eq!(
            parent.thread.unwrap().replies,
            2,
            "counted once: one from history, this one"
        );
    }

    #[test]
    fn live_edits_deletions_and_reactions_reach_open_threads() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        app.update(key(KeyCode::Enter));
        let result = Ok(vec![message(5, "U2", RichText::plain("hi")), reply(6, 5)]);
        app.update(Event::Slack(Update::Replies {
            conversation: c("C2"),
            parent: seconds(5),
            result,
        }));
        let live = |event| Event::Slack(Update::Live(event));

        let edited = Message {
            edited: true,
            ..reply(6, 5)
        };
        app.update(live(LiveEvent::Edited {
            conversation: c("C2"),
            message: edited,
        }));
        let wave = slackers_model::Emoji::named("wave");
        app.update(live(LiveEvent::Reacted {
            conversation: c("C2"),
            ts: seconds(5),
            emoji: wave,
            mine: false,
            change: slackers_model::ReactionChange::Added,
        }));
        let thread = |app: &App| app.workspace().unwrap().threads[&(c("C2"), seconds(5))].clone();
        assert!(thread(&app).get(seconds(6)).unwrap().edited);
        assert_eq!(thread(&app).get(seconds(5)).unwrap().reactions.len(), 1);
        let channel_parent = app.workspace().unwrap().timelines[&c("C2")]
            .get(seconds(5))
            .unwrap()
            .clone();
        assert_eq!(channel_parent.reactions.len(), 1, "the channel's copy too");

        app.update(live(LiveEvent::Deleted {
            conversation: c("C2"),
            ts: seconds(6),
        }));
        assert!(thread(&app).get(seconds(6)).is_none());
    }

    #[test]
    fn opening_another_conversation_closes_the_thread() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        app.update(key(KeyCode::Enter));
        keys(&mut app, "/gen");
        app.update(key(KeyCode::Enter));
        assert_eq!((app.open_thread, &app.open), (None, &Some(c("C1"))));
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
        app.update(post("C1", reply(11, 1)));
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
                thread: None,
                text,
                id: 0
            })]
        );
        assert_eq!(app.outbox.len(), 1);
        assert!(app.drafts[&place("C2", None)].is_empty());
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
        assert_eq!(timeline.newest(), Some(seconds(9)));

        keys(&mut app, "third");
        app.update(Event::Slack(Update::Posted {
            id: 1,
            conversation: c("C2"),
            result: Err(Error::SignedOut),
        }));
        assert!(app.outbox.is_empty());
        assert_eq!(app.drafts[&place("C2", None)].lines(), ["second", "third"]);
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
                place: place("C2", None),
                text: "draft".into()
            }]
        );
        app.update(Event::Edited {
            place: place("C2", None),
            outcome: Ok("edited\ntext".into()),
        });
        assert_eq!(app.drafts[&place("C2", None)].lines(), ["edited", "text"]);
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

        app.update(Event::Slack(Update::User(User {
            id: UserId::new("U9"),
            name: "Rand Al'Thor".into(),
            avatar: None,
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
