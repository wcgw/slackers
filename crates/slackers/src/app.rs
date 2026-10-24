//! The app's state, and how events change it. No I/O here: every effect is a [`Command`] for the runtime to carry
//! out, so that `update` can be tested on its own.
//!
//! Keys work as in vim: Normal mode moves around (`j`/`k` from row to row in the sidebar and from Message to Message
//! beside it, `Enter` opens a Conversation or a Thread, `za`, `zo`, `zc`, `zM` and `zR` fold the sidebar's
//! sections, `\` jumps to the sidebar), `i` enters Insert mode to write in the open Conversation or Thread, `/`
//! searches the sidebar, and `:` enters Command mode: `:q` quits, `:vw add NAME` puts the open Channel in the
//! Virtual Workspace called NAME, and `:vw rm NAME` takes it out.

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

use crate::config::{ChannelRef, VirtualWorkspace};
use crate::emoji::Emojis;
use crate::pane::{Pane, Settled};
use crate::pictures::Pictures;
use crate::search::Finder;
use crate::sidebar::{Fold, Row, Section, Sidebar};

/// Something that happened, for [`App::update`] to react to.
#[derive(Debug)]
pub(crate) enum Event {
    Terminal(TerminalEvent),
    /// News from a Workspace's Session.
    Slack(WorkspaceKey, Update),
    /// Signing in to `workspace` can't even start, for `reason`: its credentials aren't in the keyring, say.
    CantSignIn {
        workspace: WorkspaceKey,
        reason: String,
    },
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
    /// Saving the Virtual Workspaces, as [`Command::SaveVirtualWorkspaces`] asked, failed for this reason.
    SaveFailed(String),
    /// What the last draw found; see [`Drawn`].
    Drawn(Drawn),
}

/// What a draw found out that only drawing can tell.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Drawn {
    /// Pictures it would have shown, had they been loaded.
    pub(crate) wanted_pictures: Vec<WantedPicture>,
    /// When something drawn next changes frame, as animation time; `None` when nothing animates.
    pub(crate) next_change: Option<Duration>,
    /// Where the open Conversation's pane settled; `None` when it wasn't on screen.
    pub(crate) conversation: Option<Settled>,
    /// Where the open Thread's pane settled; `None` when it wasn't on screen.
    pub(crate) thread: Option<Settled>,
}

/// A picture to fetch, and the Workspace to fetch it through: Slack serves a Workspace's files to its members only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WantedPicture {
    pub(crate) workspace: WorkspaceKey,
    pub(crate) url: String,
}

/// Something for the runtime to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Ask a Workspace's Session for something.
    Slack(WorkspaceKey, Request),
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
    /// Write these Virtual Workspaces to the config file in place of those there, keeping the rest of it, and report
    /// a failure with [`Event::SaveFailed`].
    SaveVirtualWorkspaces(Vec<VirtualWorkspace>),
    Quit,
}

/// A Workspace, by its place among those the user signs in to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WorkspaceKey(usize);

#[cfg(test)]
impl WorkspaceKey {
    /// The `index`-th Workspace signed in to, counting from 0.
    pub(crate) fn nth(index: usize) -> Self {
        Self(index)
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// A Conversation in one of the Workspaces signed in to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConversationRef {
    pub(crate) workspace: WorkspaceKey,
    pub(crate) id: ConversationId,
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
    /// The matching Conversations, best first.
    pub(crate) hits: Vec<Found>,
    /// The hit under the cursor.
    pub(crate) selected: usize,
    /// How many Conversations it looked through.
    pub(crate) searched: usize,
}

/// A Conversation that matches a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) conversation: ConversationRef,
    /// Which characters of its name matched, by char index, ascending.
    pub(crate) positions: Vec<usize>,
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
    pub(crate) conversation: ConversationRef,
    /// The Thread, by its parent; `None` for the Conversation itself.
    pub(crate) thread: Option<Ts>,
}

/// The first key of a two-key command, pressed last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prefix {
    /// `z` in Normal mode, starting a fold command.
    Z,
    /// `Ctrl-x` in Insert mode, starting `Ctrl-x Ctrl-e`.
    CtrlX,
}

/// The Workspaces the user signs in to, in the order the config lists them.
#[derive(Debug)]
pub(crate) struct Workspaces(Vec<SignIn>);

/// A Workspace the user signs in to, and how far that got.
#[derive(Debug)]
pub(crate) struct SignIn {
    /// What the config and the keyring call it.
    pub(crate) label: String,
    pub(crate) phase: Phase,
}

/// Where signing in to a Workspace stands.
#[derive(Debug)]
pub(crate) enum Phase {
    SigningIn,
    Ready(Box<Workspace>),
    /// It failed, for this reason.
    Failed(String),
}

impl Workspaces {
    /// Every Workspace, signed in to or not, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (WorkspaceKey, &SignIn)> {
        self.0
            .iter()
            .enumerate()
            .map(|(index, sign_in)| (WorkspaceKey(index), sign_in))
    }

    /// The Workspace at `key`, signed in to or not.
    pub(crate) fn sign_in(&self, key: WorkspaceKey) -> Option<&SignIn> {
        self.0.get(key.0)
    }

    /// The Workspace at `key`, once signed in to.
    pub(crate) fn get(&self, key: WorkspaceKey) -> Option<&Workspace> {
        match &self.sign_in(key)?.phase {
            Phase::Ready(workspace) => Some(workspace),
            Phase::SigningIn | Phase::Failed(_) => None,
        }
    }

    fn get_mut(&mut self, key: WorkspaceKey) -> Option<&mut Workspace> {
        match &mut self.0.get_mut(key.0)?.phase {
            Phase::Ready(workspace) => Some(workspace),
            Phase::SigningIn | Phase::Failed(_) => None,
        }
    }

    fn set_phase(&mut self, key: WorkspaceKey, phase: Phase) {
        if let Some(sign_in) = self.0.get_mut(key.0) {
            sign_in.phase = phase;
        }
    }

    /// `conversation` and its Workspace, once signed in to.
    pub(crate) fn conversation(
        &self,
        conversation: &ConversationRef,
    ) -> Option<(&Workspace, &Conversation)> {
        let workspace = self.get(conversation.workspace)?;
        Some((workspace, workspace.conversation(&conversation.id)?))
    }

    /// `conversation`'s Activity, as far as known.
    pub(crate) fn activity(&self, conversation: &ConversationRef) -> Option<&Activity> {
        self.get(conversation.workspace)?
            .activity
            .get(&conversation.id)
    }

    /// The Channel `channel` names, once its Workspace is signed in to and lists it.
    fn resolve(&self, channel: &ChannelRef) -> Option<ConversationRef> {
        let (key, _) = self
            .iter()
            .find(|(_, sign_in)| sign_in.label == channel.workspace)?;
        let found = self.get(key)?.conversations.iter().find(|c| {
            matches!(c.kind, ConversationKind::Channel { .. }) && c.name == channel.channel
        })?;
        Some(ConversationRef {
            workspace: key,
            id: found.id.clone(),
        })
    }

    /// The Channel at `conversation`, as the config names it: only a Channel can be in a Virtual Workspace.
    ///
    /// # Errors
    ///
    /// Says why `conversation` can't be: it's a DM, or isn't known.
    fn channel_ref(&self, conversation: &ConversationRef) -> Result<ChannelRef, String> {
        let (_, found) = self
            .conversation(conversation)
            .ok_or("open a channel first")?;
        match found.kind {
            ConversationKind::Channel { .. } => Ok(ChannelRef {
                workspace: self
                    .sign_in(conversation.workspace)
                    .ok_or("open a channel first")?
                    .label
                    .clone(),
                channel: found.name.clone(),
            }),
            ConversationKind::Dm { .. } | ConversationKind::GroupDm => {
                Err("DMs stay with their workspace: only channels join a virtual workspace".into())
            }
        }
    }
}

/// A Workspace signed in to, as the user sees it.
#[derive(Debug)]
pub(crate) struct Workspace {
    /// What Slack calls it.
    pub(crate) name: String,
    /// Where its icon is; `None` when it has none of its own.
    pub(crate) icon: Option<String>,
    pub(crate) me: UserId,
    /// Its live connection; `None` until that first comes up.
    pub(crate) connection: Option<Connection>,
    /// Its people, as far as they've been looked up.
    pub(crate) users: HashMap<UserId, User>,
    requested_users: HashSet<UserId>,
    pub(crate) emojis: Emojis,
    /// Channels by name, then DMs, most recently active first.
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
    pub(crate) workspaces: Workspaces,
    /// The Virtual Workspaces, as the config defines them.
    pub(crate) virtual_workspaces: Vec<VirtualWorkspace>,
    pub(crate) sidebar: Sidebar,
    /// The latest thing worth telling the user, usually a problem.
    pub(crate) notice: Option<String>,
    /// Custom Emoji, avatars and Workspace icons, fetched as they come on screen.
    pub(crate) pictures: Pictures,
    pub(crate) mode: Mode,
    pub(crate) open: Option<ConversationRef>,
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
    /// The start of a two-key command, if the last key was one.
    pending: Option<Prefix>,
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
    /// An app signing in to the Workspaces with these `labels`, in this order, and listing `virtual_workspaces`.
    pub(crate) fn new(labels: Vec<String>, virtual_workspaces: Vec<VirtualWorkspace>) -> Self {
        let sign_ins = labels
            .into_iter()
            .map(|label| SignIn {
                label,
                phase: Phase::SigningIn,
            })
            .collect();
        let mut app = Self {
            workspaces: Workspaces(sign_ins),
            virtual_workspaces,
            sidebar: Sidebar::default(),
            notice: None,
            pictures: Pictures::default(),
            mode: Mode::Normal,
            open: None,
            conversation_pane: Pane::default(),
            open_thread: None,
            thread_pane: Pane::default(),
            focus: Focus::Sidebar,
            sidebar_shown: true,
            drafts: HashMap::new(),
            outbox: Vec::new(),
            next_post: 0,
            pending: None,
            focused: true,
            conversation_shown: false,
            finder: Finder::new(),
            next_change: None,
            epoch: Instant::now(),
        };
        app.list_sidebar();
        app
    }

    /// Every Workspace, signed in to or not, in order.
    pub(crate) fn workspace_keys(&self) -> impl Iterator<Item = WorkspaceKey> + use<> {
        (0..self.workspaces.0.len()).map(WorkspaceKey)
    }

    /// Time since animations started.
    pub(crate) fn animation_time(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// When the screen next needs redrawing for animation alone, as animation time.
    pub(crate) fn next_change(&self) -> Option<Duration> {
        self.next_change
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
            Event::Slack(workspace, update) => self.slack(workspace, update, &mut commands),
            Event::CantSignIn { workspace, reason } => {
                self.workspaces.set_phase(workspace, Phase::Failed(reason));
            }
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
            Event::SaveFailed(reason) => {
                self.notice = Some(format!("couldn't save the virtual workspaces: {reason}"));
            }
            Event::Drawn(drawn) => {
                for wanted in drawn.wanted_pictures {
                    if self.pictures.request(&wanted.url) {
                        let request = Request::Download { url: wanted.url };
                        commands.push(Command::Slack(wanted.workspace, request));
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
        let pending = self.pending.take();
        match self.mode {
            Mode::Normal => self.normal_key(key, pending, commands),
            Mode::Insert => self.insert_key(key, pending, commands),
            Mode::Command(_) => self.command_key(key, commands),
            Mode::Search(_) => self.search_key(key, commands),
        }
    }

    fn normal_key(&mut self, key: KeyEvent, pending: Option<Prefix>, commands: &mut Vec<Command>) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if pending == Some(Prefix::Z) {
            let fold = match key.code {
                KeyCode::Char('a') => Some(Fold::Toggle),
                KeyCode::Char('o') => Some(Fold::Open),
                KeyCode::Char('c') => Some(Fold::Close),
                KeyCode::Char('R') => Some(Fold::OpenAll),
                KeyCode::Char('M') => Some(Fold::CloseAll),
                _ => None,
            };
            if let Some(fold) = fold {
                self.sidebar.fold(fold);
            }
            return;
        }
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
                    searched: 0,
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
                KeyCode::Char('z') => self.pending = Some(Prefix::Z),
                KeyCode::Char('j') | KeyCode::Down => self.sidebar.move_by(1),
                KeyCode::Char('k') | KeyCode::Up => self.sidebar.move_by(-1),
                KeyCode::Char('g') | KeyCode::Home => self.sidebar.first(),
                KeyCode::Char('G') | KeyCode::End => self.sidebar.last(),
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                    match self.sidebar.selected().map(Row::conversation) {
                        Some(Some(conversation)) => {
                            self.open(conversation.clone(), commands);
                            self.focus = Focus::Timeline;
                        }
                        Some(None) => self.sidebar.fold(Fold::Toggle),
                        None => {}
                    }
                }
                _ => {}
            },
            Focus::Timeline => match key.code {
                KeyCode::Enter => self.open_selected_thread(commands),
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => self.focus_sidebar(),
                _ => {
                    let Some(open) = &self.open else { return };
                    let timeline = self
                        .workspaces
                        .get(open.workspace)
                        .and_then(|w| w.timelines.get(&open.id));
                    if let Some(timeline) = timeline {
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
                    let (Some(open), Some(parent)) = (&self.open, self.open_thread) else {
                        return;
                    };
                    let thread = self
                        .workspaces
                        .get(open.workspace)
                        .and_then(|w| w.threads.get(&(open.id.clone(), parent)));
                    if let Some(thread) = thread {
                        move_in(&mut self.thread_pane, thread, key);
                    }
                }
            },
        }
    }

    fn insert_key(&mut self, key: KeyEvent, pending: Option<Prefix>, commands: &mut Vec<Command>) {
        let Some(place) = self.writing_place() else {
            self.mode = Mode::Normal;
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let draft = |drafts: &mut HashMap<Place, TextArea<'static>>, place: &Place| {
            drafts
                .entry(place.clone())
                .or_insert_with(|| draft_of("", place))
                .lines()
                .join("\n")
        };
        match (key.code, control) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => self.mode = Mode::Normal,
            (KeyCode::Char('x'), true) => self.pending = Some(Prefix::CtrlX),
            (KeyCode::Char('e'), true) if pending == Some(Prefix::CtrlX) => {
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
                self.run(&line, commands);
            }
            (KeyCode::Char(c), false) => line.push(c),
            _ => {}
        }
    }

    /// Carries out `line`, a command typed after `:`.
    fn run(&mut self, line: &str, commands: &mut Vec<Command>) {
        let line = line.trim();
        let (verb, arguments) = split_word(line);
        match (verb, arguments) {
            ("", _) => {}
            ("q" | "q!" | "qa" | "qa!" | "quit" | "quitall", "") => commands.push(Command::Quit),
            ("vw", arguments) => {
                let (verb, name) = split_word(arguments);
                let changed = match (verb, name) {
                    ("add", name) if !name.is_empty() => self.add_to_virtual_workspace(name),
                    ("rm", name) if !name.is_empty() => self.remove_from_virtual_workspace(name),
                    _ => Err("usage: :vw add NAME, or :vw rm NAME".to_owned()),
                };
                match changed {
                    Ok(()) => {
                        self.list_sidebar();
                        let saved = self.virtual_workspaces.clone();
                        commands.push(Command::SaveVirtualWorkspaces(saved));
                    }
                    Err(reason) => self.notice = Some(reason),
                }
            }
            _ => self.notice = Some(format!("not a command: :{line}")),
        }
    }

    /// Puts the open Channel in the Virtual Workspace called `name` (in any case), creating it if there's none.
    ///
    /// # Errors
    ///
    /// Says why not: no Channel is open, or it's already there.
    fn add_to_virtual_workspace(&mut self, name: &str) -> Result<(), String> {
        let open = self.open.as_ref().ok_or("open a channel first")?;
        let channel = self.workspaces.channel_ref(open)?;
        let existing = self
            .virtual_workspaces
            .iter()
            .position(|virtual_workspace| same_name(&virtual_workspace.name, name));
        match existing {
            Some(at) => {
                let virtual_workspace = &mut self.virtual_workspaces[at];
                if virtual_workspace.channels.contains(&channel) {
                    return Err(format!(
                        "#{} is in {} already",
                        channel.channel, virtual_workspace.name
                    ));
                }
                virtual_workspace.channels.push(channel);
            }
            None => self.virtual_workspaces.push(VirtualWorkspace {
                name: name.to_owned(),
                channels: vec![channel],
            }),
        }
        Ok(())
    }

    /// Takes the open Channel out of the Virtual Workspace called `name` (in any case), and drops the Virtual
    /// Workspace once it holds nothing.
    ///
    /// # Errors
    ///
    /// Says why not: no Channel is open, there's no such Virtual Workspace, or the Channel isn't in it.
    fn remove_from_virtual_workspace(&mut self, name: &str) -> Result<(), String> {
        let open = self.open.as_ref().ok_or("open a channel first")?;
        let channel = self.workspaces.channel_ref(open)?;
        let at = self
            .virtual_workspaces
            .iter()
            .position(|virtual_workspace| same_name(&virtual_workspace.name, name))
            .ok_or_else(|| format!("no virtual workspace is called {name}"))?;
        let virtual_workspace = &mut self.virtual_workspaces[at];
        let Some(listed) = virtual_workspace
            .channels
            .iter()
            .position(|c| *c == channel)
        else {
            return Err(format!(
                "#{} isn't in {}",
                channel.channel, virtual_workspace.name
            ));
        };
        virtual_workspace.channels.remove(listed);
        if virtual_workspace.channels.is_empty() {
            self.virtual_workspaces.remove(at);
        }
        Ok(())
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
                let chosen = search
                    .hits
                    .get(search.selected)
                    .map(|hit| hit.conversation.clone());
                let query = std::mem::take(&mut search.query);
                self.mode = Mode::Normal;
                match chosen {
                    Some(conversation) => {
                        self.sidebar.select(&conversation);
                        self.open(conversation, commands);
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

    /// Re-ranks the sidebar's Conversations against the search query, if a search is under way. Each is known by its
    /// name followed by its Workspace's, so that either narrows the search down.
    fn refresh_search(&mut self) {
        let Mode::Search(search) = &mut self.mode else {
            return;
        };
        let candidates = self.sidebar.conversations();
        let names: Vec<String> = candidates
            .iter()
            .map(|candidate| match self.workspaces.conversation(candidate) {
                Some((workspace, conversation)) => {
                    let name = workspace.conversation_name(conversation);
                    format!("{name} {}", workspace.name)
                }
                None => String::new(),
            })
            .collect();
        search.hits = self
            .finder
            .rank(&search.query, names.iter().map(String::as_str))
            .into_iter()
            .map(|hit| Found {
                conversation: candidates[hit.index].clone(),
                positions: hit.positions,
            })
            .collect();
        search.searched = candidates.len();
        search.selected = search.selected.min(search.hits.len().saturating_sub(1));
    }

    /// Opens the Thread of the open Conversation's selected Message, in its own pane, and asks for its Replies.
    /// Any Message can start a Thread: one without Replies yet opens empty, ready for one.
    fn open_selected_thread(&mut self, commands: &mut Vec<Command>) {
        let Some(open) = &self.open else { return };
        let Some(workspace) = self.workspaces.get_mut(open.workspace) else {
            return;
        };
        let selected = workspace
            .timelines
            .get(&open.id)
            .and_then(|t| self.conversation_pane.selected(t));
        let Some(parent) = selected else { return };
        self.open_thread = Some(parent);
        self.thread_pane = Pane::default();
        self.focus = Focus::Thread;
        let thread = (open.id.clone(), parent);
        if !workspace.threads.contains_key(&thread) && workspace.loading_threads.insert(thread) {
            let request = Request::Replies {
                conversation: open.id.clone(),
                parent,
            };
            commands.push(Command::Slack(open.workspace, request));
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
        let request = Request::Post {
            conversation: place.conversation.id.clone(),
            thread: place.thread,
            text: text.clone(),
            id,
        };
        commands.push(Command::Slack(place.conversation.workspace, request));
        self.outbox.push(Outgoing { id, place, text });
    }

    /// Moves the open Conversation's Read Marker to its newest Message, if that's on screen: the window has focus,
    /// the Conversation's pane shows, and the newest Message is selected at the bottom.
    fn mark_seen(&mut self, commands: &mut Vec<Command>) {
        if !self.focused || !self.conversation_shown || !self.conversation_pane.at_newest() {
            return;
        }
        let Some(open) = &self.open else { return };
        let Some(workspace) = self.workspaces.get_mut(open.workspace) else {
            return;
        };
        let newest = workspace.timelines.get(&open.id).and_then(Timeline::newest);
        let (Some(newest), Some(activity)) = (newest, workspace.activity.get_mut(&open.id)) else {
            return;
        };
        if newest > activity.read_up_to() {
            activity.mark_read(newest);
            let request = Request::Mark {
                conversation: open.id.clone(),
                up_to: newest,
            };
            commands.push(Command::Slack(open.workspace, request));
        }
    }

    /// Moves focus to the sidebar, showing it if it's hidden.
    fn focus_sidebar(&mut self) {
        self.sidebar_shown = true;
        self.focus = Focus::Sidebar;
    }

    fn open(&mut self, conversation: ConversationRef, commands: &mut Vec<Command>) {
        self.conversation_pane = Pane::default();
        self.conversation_shown = false;
        self.open_thread = None;
        if let Some(workspace) = self.workspaces.get_mut(conversation.workspace)
            && !workspace.timelines.contains_key(&conversation.id)
            && workspace.loading.insert(conversation.id.clone())
        {
            let request = Request::History {
                conversation: conversation.id.clone(),
                before: None,
            };
            commands.push(Command::Slack(conversation.workspace, request));
        }
        self.open = Some(conversation);
    }

    /// Asks for the page before the oldest Message of the open Timeline, if there is one and it isn't on its way.
    fn load_older(&mut self, commands: &mut Vec<Command>) {
        let Some(open) = &self.open else { return };
        let Some(workspace) = self.workspaces.get_mut(open.workspace) else {
            return;
        };
        let Some(timeline) = workspace.timelines.get(&open.id) else {
            return;
        };
        if timeline.has_older() && workspace.loading.insert(open.id.clone()) {
            let request = Request::History {
                conversation: open.id.clone(),
                before: timeline.oldest(),
            };
            commands.push(Command::Slack(open.workspace, request));
        }
    }

    /// Lists the sidebar's sections afresh: each Virtual Workspace with its Channels, in the config's order, then each
    /// Workspace with its Conversations in no Virtual Workspace.
    fn list_sidebar(&mut self) {
        let mut in_virtual_workspaces = HashSet::new();
        let mut sections = Vec::new();
        for virtual_workspace in &self.virtual_workspaces {
            let channels: Vec<ConversationRef> = virtual_workspace
                .channels
                .iter()
                .filter_map(|channel| self.workspaces.resolve(channel))
                .collect();
            in_virtual_workspaces.extend(channels.iter().cloned());
            let section = Section::VirtualWorkspace(virtual_workspace.name.clone());
            sections.push((section, channels));
        }
        for (key, sign_in) in self.workspaces.iter() {
            let conversations = match &sign_in.phase {
                Phase::Ready(workspace) => workspace
                    .conversations
                    .iter()
                    .map(|c| ConversationRef {
                        workspace: key,
                        id: c.id.clone(),
                    })
                    .filter(|c| !in_virtual_workspaces.contains(c))
                    .collect(),
                Phase::SigningIn | Phase::Failed(_) => Vec::new(),
            };
            sections.push((Section::Workspace(key), conversations));
        }
        self.sidebar.list(sections);
    }

    fn slack(&mut self, key: WorkspaceKey, update: Update, commands: &mut Vec<Command>) {
        match update {
            Update::Ready(snapshot) => self.ready(key, *snapshot, commands),
            Update::SignInFailed(error) => {
                self.workspaces
                    .set_phase(key, Phase::Failed(error.to_string()));
            }
            Update::CustomEmoji(custom) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.emojis.set_custom(custom);
                }
            }
            Update::History {
                conversation,
                result,
            } => {
                let Some(workspace) = self.workspaces.get_mut(key) else {
                    return;
                };
                workspace.loading.remove(&conversation);
                match result {
                    Ok(page) => {
                        let people: Vec<UserId> =
                            page.messages.iter().flat_map(people_in).collect();
                        let timeline = workspace.timelines.entry(conversation).or_default();
                        timeline.add_history(page.messages, page.has_more);
                        workspace.want_users(key, people, commands);
                    }
                    Err(error) => self.notice = Some(describe(&error)),
                }
            }
            Update::Replies {
                conversation,
                parent,
                result,
            } => {
                let Some(workspace) = self.workspaces.get_mut(key) else {
                    return;
                };
                let thread = (conversation, parent);
                workspace.loading_threads.remove(&thread);
                match result {
                    Ok(messages) => {
                        let people: Vec<UserId> = messages.iter().flat_map(people_in).collect();
                        workspace
                            .threads
                            .entry(thread)
                            .or_insert_with(|| Timeline::thread(parent))
                            .add_history(messages, false);
                        workspace.want_users(key, people, commands);
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
                        let Some(workspace) = self.workspaces.get_mut(key) else {
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
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.users.insert(user.id.clone(), user);
                }
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
            Update::Live(event) => self.live(key, event, commands),
            Update::Connection(connection) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.connection = Some(connection);
                }
            }
        }
    }

    fn ready(&mut self, key: WorkspaceKey, snapshot: Snapshot, commands: &mut Vec<Command>) {
        let mut workspace = Workspace::new(snapshot);
        let partners: Vec<UserId> = workspace
            .conversations
            .iter()
            .filter_map(|c| match &c.kind {
                ConversationKind::Dm { with } => Some(with.clone()),
                ConversationKind::Channel { .. } | ConversationKind::GroupDm => None,
            })
            .collect();
        workspace.want_users(key, partners, commands);
        self.workspaces
            .set_phase(key, Phase::Ready(Box::new(workspace)));

        let Some(label) = self.workspaces.sign_in(key).map(|s| s.label.as_str()) else {
            return;
        };
        let missing = self
            .virtual_workspaces
            .iter()
            .flat_map(|virtual_workspace| {
                virtual_workspace
                    .channels
                    .iter()
                    .filter(|channel| channel.workspace == label)
                    .filter(|channel| self.workspaces.resolve(channel).is_none())
                    .map(move |channel| (&virtual_workspace.name, channel))
            });
        for (name, channel) in missing {
            tracing::warn!(virtual_workspace = %name, %channel, "not among the user's channels");
            self.notice = Some(format!(
                "{name} lists {channel}, which isn't among your channels"
            ));
        }

        self.list_sidebar();
        // Put the cursor on the first Conversation with something unread, without opening it: opening marks it
        // read, which the user should choose.
        let workspaces = &self.workspaces;
        self.sidebar.suggest(|row| {
            row.conversation()
                .and_then(|conversation| workspaces.activity(conversation))
                .is_some_and(Activity::has_unread)
        });
    }

    fn live(&mut self, key: WorkspaceKey, event: LiveEvent, commands: &mut Vec<Command>) {
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return;
        };
        match event {
            LiveEvent::Posted {
                conversation,
                message,
            } => {
                let people = people_in(&message);
                let is_dm = workspace
                    .conversation(&conversation)
                    .is_some_and(|c| matches!(c.kind, ConversationKind::Dm { .. }));
                if message.reply_to.is_none()
                    && !message.is_by(&workspace.me)
                    && let Some(activity) = workspace.activity.get_mut(&conversation)
                {
                    // Every DM Message counts as a Mention, as Slack's badges do.
                    activity.record(message.ts, is_dm || message.mentions(&workspace.me));
                }
                workspace.add(&conversation, message);
                workspace.want_users(key, people, commands);
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
}

impl Workspace {
    /// The Workspace `snapshot` shows, with its Conversations in sidebar order: Channels by name, then DMs with the
    /// most recent first, as Slack's sidebar does by default.
    fn new(snapshot: Snapshot) -> Self {
        let mut conversations = snapshot.conversations;
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
        let activity = conversations
            .iter()
            .map(|(c, a)| (c.id.clone(), a.clone()))
            .collect();
        Self {
            name: snapshot.name,
            icon: snapshot.icon,
            me: snapshot.me,
            connection: None,
            users: HashMap::new(),
            requested_users: HashSet::new(),
            emojis: Emojis::default(),
            conversations: conversations.into_iter().map(|(c, _)| c).collect(),
            activity,
            timelines: HashMap::new(),
            threads: HashMap::new(),
            loading: HashSet::new(),
            loading_threads: HashSet::new(),
        }
    }

    /// The Conversation with `id`, if the Workspace lists it.
    pub(crate) fn conversation(&self, id: &ConversationId) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.id == *id)
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
        match &conversation.kind {
            ConversationKind::Dm { with } => self.user_name(with),
            ConversationKind::Channel { .. } | ConversationKind::GroupDm => &conversation.name,
        }
    }

    /// Asks this Workspace, at `key`, for the names of `people` not known or asked for yet.
    fn want_users(
        &mut self,
        key: WorkspaceKey,
        people: impl IntoIterator<Item = UserId>,
        commands: &mut Vec<Command>,
    ) {
        for person in people {
            if !self.users.contains_key(&person) && self.requested_users.insert(person.clone()) {
                commands.push(Command::Slack(key, Request::User(person)));
            }
        }
    }

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

/// `text`'s first word, and the rest without leading spaces.
fn split_word(text: &str) -> (&str, &str) {
    match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim_start()),
        None => (text, ""),
    }
}

/// Whether two Virtual Workspaces' names are the same, ignoring case.
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
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

    const W0: WorkspaceKey = WorkspaceKey(0);
    const W1: WorkspaceKey = WorkspaceKey(1);

    fn conversation(id: &str, kind: ConversationKind, name: &str) -> Conversation {
        Conversation {
            id: ConversationId::new(id),
            kind,
            name: name.into(),
        }
    }

    fn channel(id: &str, name: &str) -> Conversation {
        conversation(id, ConversationKind::Channel { private: false }, name)
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

    /// Types `line` in Command mode and presses Enter.
    fn command(app: &mut App, line: &str) -> Vec<Command> {
        keys(app, &format!(":{line}"));
        app.update(key(KeyCode::Enter))
    }

    fn control(c: char) -> Event {
        Event::Terminal(TerminalEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        )))
    }

    /// The Conversation `id` in the first Workspace.
    fn c(id: &str) -> ConversationRef {
        ConversationRef {
            workspace: W0,
            id: ConversationId::new(id),
        }
    }

    fn id(id: &str) -> ConversationId {
        ConversationId::new(id)
    }

    /// News from the first Workspace.
    fn slack(update: Update) -> Event {
        Event::Slack(W0, update)
    }

    /// A request to the first Workspace.
    fn ask(request: Request) -> Command {
        Command::Slack(W0, request)
    }

    fn place(conversation: &str, thread: Option<u64>) -> Place {
        Place {
            conversation: c(conversation),
            thread: thread.map(seconds),
        }
    }

    fn workspace(app: &App, key: WorkspaceKey) -> &Workspace {
        app.workspaces.get(key).unwrap()
    }

    /// The Conversation under the sidebar's cursor.
    fn selected(app: &App) -> Option<ConversationRef> {
        app.sidebar.selected()?.conversation().cloned()
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

    fn snapshot(name: &str, conversations: Vec<(Conversation, Activity)>) -> Snapshot {
        Snapshot {
            workspace: WorkspaceId::new("T1"),
            name: name.into(),
            icon: None,
            me: UserId::new("UME"),
            conversations,
        }
    }

    /// The first Workspace's Conversations: two Channels and a DM, `#random` having unread Messages.
    fn test_snapshot() -> Snapshot {
        let dm = ConversationKind::Dm {
            with: UserId::new("U9"),
        };
        snapshot(
            "Test",
            vec![
                (conversation("D1", dm, ""), Activity::default()),
                (
                    channel("C2", "random"),
                    Activity::counted(seconds(1), seconds(5), true, 0),
                ),
                (
                    conversation("C1", ConversationKind::Channel { private: true }, "general"),
                    Activity::default(),
                ),
            ],
        )
    }

    /// An app signed in to one Workspace, `test`, which [`test_snapshot`] shows.
    fn ready_app() -> (App, Vec<Command>) {
        let mut app = App::new(vec!["test".into()], Vec::new());
        let commands = app.update(slack(Update::Ready(Box::new(test_snapshot()))));
        (app, commands)
    }

    /// An app signed in to two Workspaces: `test`, as [`test_snapshot`] shows it, and `other`, with `#general` and
    /// `#dev`. The Virtual Workspace `K` holds `test#random` and `other#dev`.
    fn two_workspaces() -> App {
        let virtual_workspaces = vec![VirtualWorkspace {
            name: "K".into(),
            channels: vec![
                ChannelRef {
                    workspace: "test".into(),
                    channel: "random".into(),
                },
                ChannelRef {
                    workspace: "other".into(),
                    channel: "dev".into(),
                },
            ],
        }];
        let mut app = App::new(vec!["test".into(), "other".into()], virtual_workspaces);
        app.update(slack(Update::Ready(Box::new(test_snapshot()))));
        let other = snapshot(
            "Other",
            vec![
                (channel("C5", "general"), Activity::default()),
                (channel("C6", "dev"), Activity::default()),
            ],
        );
        app.update(Event::Slack(W1, Update::Ready(Box::new(other))));
        app
    }

    /// The sidebar, as `>` and a section's name for a header, else the Conversation's name.
    fn sidebar(app: &App) -> Vec<String> {
        app.sidebar
            .rows()
            .iter()
            .map(|row| match row {
                Row::Header(Section::VirtualWorkspace(name)) => format!(">{name}"),
                Row::Header(Section::Workspace(key)) => {
                    format!(">{}", app.workspaces.sign_in(*key).unwrap().label)
                }
                Row::Conversation { conversation, .. } => {
                    let (workspace, found) = app.workspaces.conversation(conversation).unwrap();
                    workspace.conversation_name(found).to_owned()
                }
            })
            .collect()
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
        app.update(slack(Update::History {
            conversation: id("C2"),
            result: Ok(page),
        }));
        drawn(&mut app, 10);
        app
    }

    fn unread(app: &App, conversation: &str) -> bool {
        workspace(app, W0).activity[&id(conversation)].has_unread()
    }

    #[test]
    fn signing_in_sorts_the_sidebar_and_selects_the_first_unread_without_opening_it() {
        let (app, commands) = ready_app();
        assert_eq!(sidebar(&app), [">test", "general", "random", "U9"]);
        assert_eq!(selected(&app), Some(c("C2")));
        assert_eq!(app.open, None);
        assert_eq!(commands, [ask(Request::User(UserId::new("U9")))]);
    }

    #[test]
    fn virtual_workspaces_come_first_and_take_their_channels_out_of_their_workspaces() {
        let app = two_workspaces();
        assert_eq!(
            sidebar(&app),
            [
                ">K", "random", "dev", ">test", "general", "U9", ">other", "general"
            ]
        );
        assert_eq!(selected(&app), Some(c("C2")), "the unread one, in K");
    }

    #[test]
    fn workspaces_sign_in_on_their_own() {
        let mut app = App::new(vec!["test".into(), "other".into()], Vec::new());
        assert_eq!(sidebar(&app), [">test", ">other"]);
        app.update(Event::CantSignIn {
            workspace: W1,
            reason: "no token".into(),
        });
        app.update(slack(Update::Ready(Box::new(test_snapshot()))));
        let other = &app.workspaces.sign_in(W1).unwrap().phase;
        assert!(matches!(other, Phase::Failed(reason) if reason == "no token"));
        assert_eq!(
            sidebar(&app),
            [">test", "general", "random", "U9", ">other"]
        );
    }

    #[test]
    fn requests_go_to_the_conversations_workspace() {
        let mut app = two_workspaces();
        keys(&mut app, "j");
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [Command::Slack(
                W1,
                Request::History {
                    conversation: id("C6"),
                    before: None
                }
            )]
        );
        assert_eq!(
            app.open,
            Some(ConversationRef {
                workspace: W1,
                id: id("C6")
            })
        );
        let page = HistoryPage {
            messages: vec![message(5, "U7", RichText::plain("hi"))],
            has_more: false,
        };
        let commands = app.update(Event::Slack(
            W1,
            Update::History {
                conversation: id("C6"),
                result: Ok(page),
            },
        ));
        assert_eq!(
            commands,
            [Command::Slack(W1, Request::User(UserId::new("U7")))]
        );
        assert!(workspace(&app, W0).timelines.is_empty());

        let wanted = Drawn {
            wanted_pictures: vec![WantedPicture {
                workspace: W1,
                url: "https://e/p.png".into(),
            }],
            ..Drawn::default()
        };
        assert_eq!(
            app.update(Event::Drawn(wanted)),
            [Command::Slack(
                W1,
                Request::Download {
                    url: "https://e/p.png".into()
                }
            )]
        );
    }

    #[test]
    fn vw_add_puts_the_open_channel_in_a_virtual_workspace_and_saves() {
        let mut app = two_workspaces();
        keys(&mut app, "jjj");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.open, Some(c("C1")));

        let commands = command(&mut app, "vw add k");
        let saved = &app.virtual_workspaces;
        assert_eq!(saved.len(), 1, "K, in any case");
        assert_eq!(saved[0].channels[2].to_string(), "test#general");
        assert_eq!(commands, [Command::SaveVirtualWorkspaces(saved.clone())]);
        assert_eq!(
            sidebar(&app),
            [
                ">K", "random", "dev", "general", ">test", "U9", ">other", "general"
            ]
        );
        assert_eq!(app.sidebar.cursor(), 3, "the cursor follows it");

        command(&mut app, "vw add Gateway API");
        assert_eq!(app.virtual_workspaces[1].name, "Gateway API");
        assert_eq!(sidebar(&app)[4..7], [">Gateway API", "general", ">test"]);

        assert!(command(&mut app, "vw add K").is_empty());
        assert_eq!(app.notice.as_deref(), Some("#general is in K already"));
    }

    #[test]
    fn vw_rm_takes_it_out_and_an_empty_virtual_workspace_goes() {
        let mut app = two_workspaces();
        app.update(key(KeyCode::Enter));
        assert_eq!(app.open, Some(c("C2")));

        let commands = command(&mut app, "vw rm K");
        assert_eq!(app.virtual_workspaces[0].channels.len(), 1);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            sidebar(&app),
            [
                ">K", "dev", ">test", "general", "random", "U9", ">other", "general"
            ]
        );

        assert!(command(&mut app, "vw rm K").is_empty());
        assert_eq!(app.notice.as_deref(), Some("#random isn't in K"));
        keys(&mut app, "\\");
        keys(&mut app, "gj");
        app.update(key(KeyCode::Enter));
        command(&mut app, "vw rm K");
        assert!(app.virtual_workspaces.is_empty());
        assert_eq!(sidebar(&app)[0], ">test");
    }

    #[test]
    fn dms_stay_out_of_virtual_workspaces() {
        let mut app = two_workspaces();
        keys(&mut app, "jjjj");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.open, Some(c("D1")));
        assert!(command(&mut app, "vw add K").is_empty());
        assert!(app.notice.as_deref().unwrap().starts_with("DMs stay"));

        assert!(command(&mut app, "vw add").is_empty());
        assert_eq!(
            app.notice.as_deref(),
            Some("usage: :vw add NAME, or :vw rm NAME")
        );
    }

    #[test]
    fn z_commands_and_enter_fold_sections() {
        let mut app = two_workspaces();
        keys(&mut app, "zc");
        assert_eq!(
            sidebar(&app),
            [">K", ">test", "general", "U9", ">other", "general"]
        );
        assert_eq!(app.sidebar.cursor(), 0);
        app.update(key(KeyCode::Enter));
        assert_eq!(sidebar(&app).len(), 8, "enter on a header unfolds it");

        keys(&mut app, "zM");
        assert_eq!(sidebar(&app), [">K", ">test", ">other"]);
        keys(&mut app, "jza");
        assert_eq!(sidebar(&app), [">K", ">test", "general", "U9", ">other"]);
        keys(&mut app, "zR");
        assert_eq!(sidebar(&app).len(), 8);
        assert!(keys(&mut app, "zx").is_empty(), "not a fold command");
        assert_eq!(app.sidebar.cursor(), 3, "and no move either");
    }

    #[test]
    fn a_search_spans_workspaces_and_knows_them_by_name() {
        let mut app = two_workspaces();
        app.sidebar.fold(Fold::CloseAll);
        keys(&mut app, "/general");
        let hits = |app: &App| {
            let Mode::Search(search) = &app.mode else {
                panic!("not searching")
            };
            search
                .hits
                .iter()
                .map(|hit| hit.conversation.clone())
                .collect::<Vec<_>>()
        };
        let other_general = ConversationRef {
            workspace: W1,
            id: id("C5"),
        };
        assert_eq!(hits(&app), [c("C1"), other_general.clone()]);

        keys(&mut app, " oth");
        assert_eq!(hits(&app), std::slice::from_ref(&other_general));
        app.update(key(KeyCode::Enter));
        assert_eq!(app.open, Some(other_general.clone()));
        assert_eq!(
            selected(&app),
            Some(other_general),
            "its section unfolds to show it"
        );
    }

    #[test]
    fn opening_loads_history_asks_for_names_and_marks_it_read_once_shown() {
        let (mut app, _) = ready_app();
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [ask(Request::History {
                conversation: id("C2"),
                before: None
            })]
        );
        let page = HistoryPage {
            messages: vec![message(5, "U2", RichText::plain("hi"))],
            has_more: false,
        };
        let commands = app.update(slack(Update::History {
            conversation: id("C2"),
            result: Ok(page),
        }));
        assert_eq!(commands, [ask(Request::User(UserId::new("U2")))]);
        assert!(unread(&app, "C2"), "not on screen yet");

        assert_eq!(
            drawn(&mut app, 10),
            [ask(Request::Mark {
                conversation: id("C2"),
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
        app.update(slack(Update::History {
            conversation: id("C2"),
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
            [ask(Request::Mark {
                conversation: id("C2"),
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
        app.update(slack(Update::History {
            conversation: id("C2"),
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
            [ask(Request::History {
                conversation: id("C2"),
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
            let timeline = &workspace(app, W0).timelines[&id("C2")];
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
            [ask(Request::Replies {
                conversation: id("C2"),
                parent: seconds(5)
            })]
        );

        let replies = vec![message(5, "U2", RichText::plain("hi")), reply(6, 5)];
        let result = Ok(replies);
        let commands = app.update(slack(Update::Replies {
            conversation: id("C2"),
            parent: seconds(5),
            result,
        }));
        assert_eq!(commands, [ask(Request::User(UserId::new("U3")))]);
        let thread = &workspace(&app, W0).threads[&(id("C2"), seconds(5))];
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
        app.update(slack(Update::Replies {
            conversation: id("C2"),
            parent: seconds(5),
            result,
        }));

        keys(&mut app, "ion it");
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            commands,
            [ask(Request::Post {
                conversation: id("C2"),
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
        app.update(slack(Update::Posted {
            id: 0,
            conversation: id("C2"),
            result: Ok(posted.clone()),
        }));
        // The live echo of the same Reply changes nothing.
        let echo = Update::Live(LiveEvent::Posted {
            conversation: id("C2"),
            message: posted,
        });
        app.update(slack(echo));

        let workspace = workspace(&app, W0);
        assert_eq!(
            workspace.threads[&(id("C2"), seconds(5))].messages().len(),
            2
        );
        let parent = workspace.timelines[&id("C2")].get(seconds(5)).unwrap();
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
        app.update(slack(Update::Replies {
            conversation: id("C2"),
            parent: seconds(5),
            result,
        }));
        let live = |event| slack(Update::Live(event));

        let edited = Message {
            edited: true,
            ..reply(6, 5)
        };
        app.update(live(LiveEvent::Edited {
            conversation: id("C2"),
            message: edited,
        }));
        let wave = slackers_model::Emoji::named("wave");
        app.update(live(LiveEvent::Reacted {
            conversation: id("C2"),
            ts: seconds(5),
            emoji: wave,
            mine: false,
            change: slackers_model::ReactionChange::Added,
        }));
        let thread = |app: &App| workspace(app, W0).threads[&(id("C2"), seconds(5))].clone();
        assert!(thread(&app).get(seconds(6)).unwrap().edited);
        assert_eq!(thread(&app).get(seconds(5)).unwrap().reactions.len(), 1);
        let channel_parent = workspace(&app, W0).timelines[&id("C2")]
            .get(seconds(5))
            .unwrap()
            .clone();
        assert_eq!(channel_parent.reactions.len(), 1, "the channel's copy too");

        app.update(live(LiveEvent::Deleted {
            conversation: id("C2"),
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
            slack(Update::Live(LiveEvent::Posted {
                conversation: id(conversation),
                message,
            }))
        };
        app.update(post("C1", message(10, "UME", RichText::plain("mine"))));
        app.update(post("C1", reply(11, 1)));
        assert!(!unread(&app, "C1"));

        app.update(post("C1", message(12, "U2", mention)));
        app.update(post("D1", message(13, "U9", RichText::plain("dm"))));
        let mentions = |app: &App, conversation: &str| {
            workspace(app, W0).activity[&id(conversation)].mentions()
        };
        assert_eq!(mentions(&app, "C1"), 1);
        assert_eq!(mentions(&app, "D1"), 1, "DMs count as mentions");

        let marked = LiveEvent::Marked {
            conversation: id("C1"),
            up_to: seconds(12),
        };
        app.update(slack(Update::Live(marked)));
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
            [ask(Request::Post {
                conversation: id("C2"),
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
        app.update(slack(Update::Posted {
            id: 0,
            conversation: id("C2"),
            result: Ok(posted),
        }));
        let timeline = &workspace(&app, W0).timelines[&id("C2")];
        assert_eq!(timeline.newest(), Some(seconds(9)));

        keys(&mut app, "third");
        app.update(slack(Update::Posted {
            id: 1,
            conversation: id("C2"),
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

        assert_eq!(command(&mut app, "q"), [Command::Quit]);
    }

    fn hit_names(app: &App) -> Vec<String> {
        let Mode::Search(search) = &app.mode else {
            panic!("not searching: {:?}", app.mode)
        };
        search
            .hits
            .iter()
            .map(|hit| {
                let (workspace, found) = app.workspaces.conversation(&hit.conversation).unwrap();
                workspace.conversation_name(found).to_owned()
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

        app.update(slack(Update::User(User {
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
        assert_eq!(selected(&app), Some(c("D1")));
        assert_eq!(app.focus, Focus::Timeline);
        assert_eq!(
            commands,
            [ask(Request::History {
                conversation: id("D1"),
                before: None
            })]
        );
    }

    #[test]
    fn escape_and_backspacing_past_the_slash_cancel_a_search() {
        let (mut app, _) = ready_app();
        keys(&mut app, "/gen");
        app.update(key(KeyCode::Esc));
        assert_eq!((&app.mode, &app.open), (&Mode::Normal, &None));
        assert_eq!(selected(&app), Some(c("C2")));

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
        app.update(slack(Update::CustomEmoji(custom)));

        let wanted = || {
            Event::Drawn(Drawn {
                wanted_pictures: vec![WantedPicture {
                    workspace: W0,
                    url: "https://e/p.gif".into(),
                }],
                ..Drawn::default()
            })
        };
        assert_eq!(
            app.update(wanted()),
            [ask(Request::Download {
                url: "https://e/p.gif".into()
            })]
        );
        assert!(app.update(wanted()).is_empty());
        let downloaded = Update::Downloaded {
            url: "https://e/p.gif".into(),
            result: Ok(vec![1, 2]),
        };
        assert_eq!(
            app.update(slack(downloaded)),
            [Command::Prepare {
                url: "https://e/p.gif".into(),
                bytes: vec![1, 2],
                fit: Fit::Emoji
            }]
        );
    }
}
