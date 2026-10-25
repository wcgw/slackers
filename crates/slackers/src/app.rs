//! The app's state, and how events change it. No I/O here: every effect is a [`Command`] for the runtime to carry
//! out, so that `update` can be tested on its own.
//!
//! Keys work as in vim: Normal mode moves around (`j`/`k` from row to row in the sidebar and from Message to Message
//! beside it, `Enter` opens a Conversation or a Thread, `za`, `zo`, `zc`, `zM` and `zR` fold the sidebar's
//! sections, `\` jumps to the sidebar), `i` enters Insert mode to write in the open Conversation or Thread, `/`
//! searches the sidebar, and `:` enters Command mode: `:q` quits, `:vw add NAME` puts the open Channel in the
//! Virtual Workspace called NAME, and `:vw rm NAME` takes it out. In Insert mode, a word starting with `@` brings
//! up people and broadcasts to @-mention, and one starting with `:` and two more characters brings up emoji: `↑`/`↓`
//! choose one, `Tab` or `Enter` picks it.
//!
//! A Conversation, or a Followed Thread, with something unread opens on its first unread Message, under a rule
//! saying what's new, loading older history to reach it; it's read once its newest Message is reached.
//!
//! The sidebar's first row opens Activity, merged from every Workspace, newest first: `j`/`k` choose an item and
//! `Enter` opens its Message, in its Thread if it has one.
//!
//! While the terminal window doesn't have focus, a DM, a Message that mentions the user, or a Reply in one of their
//! Followed Threads shows a desktop notification.
//!
//! Whoever is typing in a pane's Conversation or Thread shows on its title row, and a DM's icon in the sidebar
//! says whether the other person is using Slack right now.
//!
//! A Draft starting with `/` and a word runs that slash command rather than posting: `/` brings up the Workspace's
//! commands to choose from, as `@` does people. Starting it with a space posts it as text instead. When the command
//! posts for the user elsewhere, as `/dm @someone hi` does, slackers follows it there.
//!
//! On a Message in a Conversation, `U` marks it and everything after it unread, as Slack's "Mark unread" does: the
//! Conversation stays unread while it's open, rather than being read again.
//!
//! On one of the user's Messages, `e` edits it in its pane's composer, as `↑` does in an empty one for their newest
//! there: `Enter` saves the edit and `Esc` gives it up, bringing back what was being written before.
//!
//! On a Message, `+` reacts: it lists the emoji the user uses most, which typing narrows down, for `↑`/`↓` to
//! choose and `Enter` or `Tab` to react with, or to take back one of the user's reactions. `K` shows its details: who
//! wrote it, who reacted, the buttons an app put in it, and where it leads (its links, its files and the Message
//! itself on Slack's web client), for `j`/`k` to choose, `Enter` or `1`-`9` to press or open and `y` to copy; with
//! more than one to choose from, they stay up for the next. `o` opens a Message's link or file when it has just one,
//! and shows its details to choose from otherwise.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use slackers_gfx::{CellSize, Fit, Picture};
use slackers_model::{
    Author, Block, BotId, Broadcast, Button, Conversation, ConversationId, ConversationKind, Emoji,
    Happened, Inline, Message, MessageRef, Presence, ReactionChange, RichText, Style, Timeline, Ts,
    Unread, UserId,
};
use slackers_slack::{
    Bot, Connection, EmojiList, Error, LiveEvent, Permalinks, Person, Request, SlashCommand,
    Snapshot, Update, User,
};

use crate::activity::{Activity, Fetched};
use crate::config::{ChannelRef, VirtualWorkspace};
use crate::draft::{Completing, Draft, Mentioned, Reference, Written};
use crate::emoji::Emojis;
use crate::pane::{Pane, Settled};
use crate::pictures::Pictures;
use crate::search::{self, Finder};
use crate::sidebar::{Fold, Item, Row, Section, Sidebar};
use crate::typing::Typing;

/// The most people and broadcasts offered to @-mention at once, and the most emoji.
const SUGGESTIONS: usize = 8;

/// The most people named per reaction in a Message's details, whose names are looked up: more wouldn't fit on the
/// reaction's line.
const NAMED_REACTORS: usize = 12;

/// The most notifications held back until the names in them are known; past that, the oldest go.
const HELD_NOTIFICATIONS: usize = 20;

/// The most pages of history loaded to reach the Message a Conversation opens on: its first unread, or the one an
/// Activity item is about.
const JUMP_PAGES: u32 = 10;

/// How long a notice shows on the status line: long enough to read one that came while the user was busy.
const NOTICE_FOR: Duration = Duration::from_secs(8);

/// How long after running a slash command a Message the user posts elsewhere counts as the command's doing, in
/// microseconds.
const COMMAND_POSTS_WITHIN: u64 = 15 * 1_000_000;

/// How far Slack's clock, which times Messages, may be behind this one, in microseconds.
const CLOCK_SKEW: u64 = 5 * 1_000_000;

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
    /// Opening a link, as [`Command::Open`] asked, failed for this reason.
    OpenFailed(String),
    /// What the last draw found; see [`Drawn`].
    Drawn(Drawn),
}

/// What a draw found out that only drawing can tell.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Drawn {
    /// The pictures it showed, and those it would have shown had they been loaded: all of them should be in the
    /// terminal.
    pub(crate) wanted_pictures: Vec<WantedPicture>,
    /// When something drawn next changes frame, as animation time; `None` when nothing animates.
    pub(crate) next_change: Option<Duration>,
    /// The Messages it would have shown for Activity items, had they been loaded, by Workspace and where they are.
    pub(crate) wanted_messages: Vec<(WorkspaceKey, MessageRef)>,
    /// Where the open Conversation's pane settled; `None` when it wasn't on screen.
    pub(crate) conversation: Option<Settled>,
    /// Where the open Thread's pane settled; `None` when it wasn't on screen.
    pub(crate) thread: Option<Settled>,
}

/// A picture to have in the terminal, the Workspace to fetch it through (Slack serves a Workspace's files to its
/// members only), and how big to draw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WantedPicture {
    pub(crate) workspace: WorkspaceKey,
    pub(crate) url: String,
    pub(crate) fit: Fit,
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
    /// Delete these pictures from the terminal, freeing its memory. Nothing draws them any more.
    Forget(Vec<Picture>),
    /// Hand the terminal to `$EDITOR` to edit `text`, the draft for `place`, then report back with
    /// [`Event::Edited`] and [`Event::PicturesLost`].
    Edit {
        place: Place,
        text: String,
    },
    /// Write these Virtual Workspaces to the config file in place of those there, keeping the rest of it, and report
    /// a failure with [`Event::SaveFailed`].
    SaveVirtualWorkspaces(Vec<VirtualWorkspace>),
    /// Open this web link in the user's browser, and report a failure with [`Event::OpenFailed`].
    Open(String),
    /// Put this text on the clipboard, through the terminal.
    Copy(String),
    /// Show a desktop notification, through the terminal.
    Notify {
        /// Who and where, in a few words.
        title: String,
        /// The gist of what.
        body: String,
    },
    Quit,
}

/// A Workspace, by its place among those the user signs in to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    /// Keys choose a button to press or somewhere to open, from a Message's details shown over its pane.
    Details(Details),
    /// Keys choose an emoji to react to a Message with, shown after `+` on the status line.
    React(Reacting),
}

/// Choosing an emoji to react to a Message with, or to take back the user's reaction with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reacting {
    /// Where the Message is held: its Conversation, or the Thread whose pane it was chosen in.
    pub(crate) place: Place,
    pub(crate) ts: Ts,
    /// The emoji's name, as far as typed.
    pub(crate) query: String,
    /// The emoji to choose from, the user's reactions among them saying so; `None` when none matches.
    pub(crate) choices: Option<Suggestions>,
}

/// A Message whose details show: who wrote it and when, who reacted, its buttons and where it leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Details {
    /// Where the Message is held: its Conversation, or the Thread whose pane it was chosen in.
    pub(crate) place: Place,
    pub(crate) ts: Ts,
    /// The choice `Enter` takes, by its place among the Message's [`Workspace::choices`].
    pub(crate) selected: usize,
}

/// What a Message's details offer to choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// One of the buttons an app put in it, to press.
    Press(Button),
    /// Somewhere it leads, to open or copy.
    Open(Link),
}

/// Somewhere a Message leads, to open in a browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) kind: LinkKind,
    /// What to call it.
    pub(crate) label: String,
    /// Where it goes: always an `http` or `https` URL.
    pub(crate) url: String,
}

/// What a [`Link`] leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// A web page linked in the Message's text.
    Web,
    /// A file shared with the Message, on Slack's web client.
    File,
    /// The Message itself, on Slack's web client.
    Message,
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

/// What to complete the word being written with, offered in Insert mode while it starts with `@` (people and
/// broadcasts to @-mention) or `:` (emoji).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Suggestions {
    /// Best first; never empty.
    pub(crate) items: Vec<Suggestion>,
    /// The one `Tab` or `Enter` picks.
    pub(crate) selected: usize,
}

/// Something to complete the word being written with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Suggestion {
    pub(crate) suggested: Suggested,
    /// What it's called: what an @-mention calls someone, or an emoji's name.
    pub(crate) name: String,
    /// More to tell it by, such as someone's full name, or another name an emoji goes by; empty when there's nothing
    /// more.
    pub(crate) detail: String,
    /// Which characters of `name` match the name being written, by char index, ascending.
    pub(crate) positions: Vec<usize>,
}

/// What picking a [`Suggestion`] writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Suggested {
    Mention(Mentioned),
    /// The emoji the suggestion names.
    Emoji,
    /// The slash command the suggestion names.
    Command,
}

impl Suggestion {
    /// Whether it writes what `other` does, though perhaps under another name.
    fn writes_as(&self, other: &Self) -> bool {
        match (&self.suggested, &other.suggested) {
            (Suggested::Mention(mentioned), Suggested::Mention(other)) => mentioned == other,
            (Suggested::Emoji, Suggested::Emoji) | (Suggested::Command, Suggested::Command) => {
                self.name == other.name
            }
            (Suggested::Mention(_) | Suggested::Emoji | Suggested::Command, _) => false,
        }
    }
}

/// An edit of one of the user's Messages, written in its Place's Draft.
#[derive(Debug)]
struct Editing {
    /// The Message.
    ts: Ts,
    /// What was being written there before, set aside until the edit is saved or given up; `None` if nothing was.
    set_aside: Option<Draft>,
}

/// Which pane Normal mode's keys move around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Sidebar,
    /// The open Conversation's Messages.
    Timeline,
    /// The open Thread's Messages.
    Thread,
    /// Activity's items.
    Activity,
}

/// What can be open beside the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Opened {
    /// A Conversation, and maybe one of its Threads beside it.
    Conversation(ConversationRef),
    /// Activity, merged from every Workspace.
    Activity,
}

impl Opened {
    /// The Conversation it is, if it is one.
    pub(crate) fn conversation(&self) -> Option<&ConversationRef> {
        match self {
            Self::Conversation(conversation) => Some(conversation),
            Self::Activity => None,
        }
    }
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

    /// What's Unread in `conversation`, as far as known.
    pub(crate) fn unread(&self, conversation: &ConversationRef) -> Option<&Unread> {
        self.get(conversation.workspace)?
            .unread
            .get(&conversation.id)
    }

    /// The Followed Thread under `parent` in `conversation`, if the user follows it.
    pub(crate) fn followed(&self, conversation: &ConversationRef, parent: Ts) -> Option<&Followed> {
        self.get(conversation.workspace)?
            .followed
            .get(&(conversation.id.clone(), parent))
    }

    /// Whether `item` has something unread: Messages in a Conversation, or Replies in a Followed Thread.
    pub(crate) fn has_unread(&self, item: &Item) -> bool {
        match item {
            Item::Conversation(conversation) => {
                self.unread(conversation).is_some_and(Unread::has_unread)
            }
            Item::Thread {
                conversation,
                parent,
            } => self
                .followed(conversation, *parent)
                .is_some_and(|followed| followed.unread.has_unread()),
        }
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
    /// Its bots that posted without saying their names, as far as they've been looked up.
    bots: HashMap<BotId, Bot>,
    requested_bots: HashSet<BotId>,
    /// Whether people are using Slack, as far as Slack said.
    presence: HashMap<UserId, Presence>,
    /// Whose presence it was last asked to follow, in sidebar order.
    presence_followed: Vec<UserId>,
    pub(crate) emojis: Emojis,
    permalinks: Permalinks,
    /// Channels by name, then DMs, most recently active first.
    pub(crate) conversations: Vec<Conversation>,
    pub(crate) unread: HashMap<ConversationId, Unread>,
    /// Conversations' top-level Messages.
    pub(crate) timelines: HashMap<ConversationId, Timeline>,
    /// Threads, by their Conversation and parent.
    pub(crate) threads: HashMap<(ConversationId, Ts), Timeline>,
    /// Conversations with a history request in flight.
    loading: HashSet<ConversationId>,
    /// Threads with a request for their Replies in flight.
    loading_threads: HashSet<(ConversationId, Ts)>,
    /// Followed Threads, by their Conversation and parent.
    pub(crate) followed: HashMap<(ConversationId, Ts), Followed>,
    /// Its slash commands, by name; empty until Slack lists them.
    pub(crate) commands: Vec<SlashCommand>,
    /// Whom Slack found by name, by the name searched for, in lower case. A failed search found no one.
    found: HashMap<String, Vec<Person>>,
    /// The name Slack is searching for, if any.
    finding: Option<String>,
}

/// A Followed Thread.
#[derive(Debug)]
pub(crate) struct Followed {
    /// Its Replies past its Read Marker, and the Mentions among them.
    pub(crate) unread: Unread,
    /// The Message that started it; `None` when it hasn't been seen.
    pub(crate) parent: Option<Message>,
}

/// A Message on its way to Slack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outgoing {
    /// Matches the [`Request::Post`] to its [`Update::Posted`].
    id: u64,
    pub(crate) place: Place,
    pub(crate) written: Written,
}

/// Everything slackers shows. Only [`App::update`] changes it; the view reads it.
#[derive(Debug)]
pub(crate) struct App {
    pub(crate) workspaces: Workspaces,
    /// The Virtual Workspaces, as the config defines them.
    pub(crate) virtual_workspaces: Vec<VirtualWorkspace>,
    pub(crate) sidebar: Sidebar,
    /// The latest thing worth telling the user, usually a problem.
    notice: Option<Notice>,
    /// Custom Emoji, avatars, Workspace icons and images, fetched as they come on screen.
    pub(crate) pictures: Pictures,
    /// How big the terminal's cells are, so that a picture takes its room before it loads.
    pub(crate) cell: CellSize,
    pub(crate) mode: Mode,
    /// What's open beside the sidebar; `None` until something is.
    pub(crate) opened: Option<Opened>,
    /// Where the user is in the open Conversation.
    pub(crate) conversation_pane: Pane,
    /// The open Thread, in the open Conversation, by its parent.
    pub(crate) open_thread: Option<Ts>,
    /// Where the user is in the open Thread.
    pub(crate) thread_pane: Pane,
    pub(crate) focus: Focus,
    /// Every Workspace's Activity, and where the user is in it.
    pub(crate) activity: Activity,
    /// Who's typing where, in every Workspace.
    pub(crate) typing: Typing,
    /// While the open Conversation's selected Message is older than its history loaded, which it opened on, how
    /// many more pages may load to reach it; `None` once it's reached, or given up on.
    reaching: Option<u32>,
    /// Whether the sidebar shows. `\` hides it from within it, and brings it back. Whenever focus is on it, it shows.
    pub(crate) sidebar_shown: bool,
    /// Unsent text, per Place.
    pub(crate) drafts: HashMap<Place, Draft>,
    /// What to complete the word being written in a Draft with, while it starts with `@` or `:`.
    pub(crate) suggestions: Option<Suggestions>,
    /// Messages posted but not yet confirmed, oldest first.
    pub(crate) outbox: Vec<Outgoing>,
    /// Slash commands run but not yet answered, by request id, to put back in their Drafts should they fail.
    commands_sent: Vec<(u64, Place, Written)>,
    /// The slash command run last, while it may yet post for the user elsewhere: slackers follows it there.
    following: Option<Following>,
    /// The open Conversation, when the user marked it unread: it stays so until they open another.
    kept_unread: Option<ConversationRef>,
    /// Edits being written, by where: in that Place's Draft, set aside meanwhile.
    editing: HashMap<Place, Editing>,
    /// Edits saved but not yet confirmed, to put back should Slack refuse them.
    edits_sent: Vec<(Place, Ts, Written)>,
    next_post: u64,
    /// The start of a two-key command, if the last key was one.
    pending: Option<Prefix>,
    /// Whether the terminal window has focus. Messages count as read only while it does, and notifications show only
    /// while it doesn't.
    focused: bool,
    /// Messages to notify of once the names of everyone in them are known, by Workspace and Conversation, oldest
    /// first; at most [`HELD_NOTIFICATIONS`].
    held_notifications: Vec<(WorkspaceKey, ConversationId, Message)>,
    /// Whether the open Conversation's pane was on screen at the last draw; a Thread may cover it.
    conversation_shown: bool,
    /// Whether the open Thread's pane was on screen at the last draw.
    thread_shown: bool,
    finder: Finder,
    /// When something drawn next changes frame, as animation time.
    next_change: Option<Duration>,
    /// When animations started; every picture is drawn against it, so copies of one emoji animate in step.
    epoch: Instant,
}

impl App {
    /// An app signing in to the Workspaces with these `labels`, in this order, and listing `virtual_workspaces`, on
    /// a terminal with cells of `cell` pixels.
    pub(crate) fn new(
        labels: Vec<String>,
        virtual_workspaces: Vec<VirtualWorkspace>,
        cell: CellSize,
    ) -> Self {
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
            cell,
            mode: Mode::Normal,
            opened: None,
            conversation_pane: Pane::default(),
            open_thread: None,
            thread_pane: Pane::default(),
            focus: Focus::Sidebar,
            activity: Activity::default(),
            typing: Typing::default(),
            reaching: None,
            sidebar_shown: true,
            drafts: HashMap::new(),
            suggestions: None,
            outbox: Vec::new(),
            commands_sent: Vec::new(),
            following: None,
            kept_unread: None,
            editing: HashMap::new(),
            edits_sent: Vec::new(),
            next_post: 0,
            pending: None,
            focused: true,
            held_notifications: Vec::new(),
            conversation_shown: false,
            thread_shown: false,
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

    /// What to tell the user as of `now`, as animation time; `None` once the latest notice has had its time.
    pub(crate) fn notice(&self, now: Duration) -> Option<&Notice> {
        self.notice.as_ref().filter(|notice| now < notice.until)
    }

    /// Tells the user `text` on the status line, for [`NOTICE_FOR`] or until something else is worth telling.
    fn tell(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            until: self.animation_time() + NOTICE_FOR,
        });
    }

    /// When the screen next needs redrawing for animation alone, as animation time.
    pub(crate) fn next_change(&self) -> Option<Duration> {
        self.next_change
    }

    /// The open Conversation, if one is.
    pub(crate) fn open_conversation(&self) -> Option<&ConversationRef> {
        self.opened.as_ref().and_then(Opened::conversation)
    }

    /// Where Insert mode writes: the open Thread when its pane has focus, else the open Conversation.
    pub(crate) fn writing_place(&self) -> Option<Place> {
        let thread = self.open_thread.filter(|_| self.focus == Focus::Thread);
        Some(Place {
            conversation: self.open_conversation()?.clone(),
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
            Event::Terminal(TerminalEvent::FocusGained) => {
                self.focused = true;
                // Back in front of the user: notifications still held back are moot.
                self.held_notifications.clear();
            }
            Event::Terminal(TerminalEvent::FocusLost) => self.focused = false,
            Event::Terminal(_) => {}
            Event::Slack(workspace, update) => self.slack(workspace, update, &mut commands),
            Event::CantSignIn { workspace, reason } => {
                self.workspaces.set_phase(workspace, Phase::Failed(reason));
            }
            Event::PictureReady { url, picture } => {
                if let Some(again) = self.pictures.loaded(url, picture) {
                    commands.push(Command::Forget(vec![again]));
                }
            }
            Event::PictureFailed { url } => self.pictures.failed(url),
            Event::PicturesLost => self.pictures.forget_all(),
            Event::Edited { place, outcome } => match outcome {
                Ok(text) => self.draft(&place).set_text(&text),
                Err(reason) => self.tell(reason),
            },
            Event::SaveFailed(reason) => {
                self.tell(format!("couldn't save the virtual workspaces: {reason}"));
            }
            Event::OpenFailed(reason) => {
                self.tell(format!("couldn't open the link: {reason}"));
            }
            Event::Drawn(drawn) => {
                for (key, at) in drawn.wanted_messages {
                    // Unless it's loaded in its Conversation or Thread.
                    if self.activity_message(key, &at) == Fetched::Unasked
                        && let Some(request) = self.activity.fetch(key, &at)
                    {
                        commands.push(Command::Slack(key, request));
                    }
                }
                for wanted in drawn.wanted_pictures {
                    if self.pictures.want(&wanted.url, wanted.fit) {
                        let request = Request::Download { url: wanted.url };
                        commands.push(Command::Slack(wanted.workspace, request));
                    }
                }
                let evicted = self.pictures.evict();
                if !evicted.is_empty() {
                    commands.push(Command::Forget(evicted));
                }
                self.next_change = drawn.next_change;
                self.conversation_shown = drawn.conversation.is_some();
                self.thread_shown = drawn.thread.is_some();
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
        self.mark_thread_seen(&mut commands);
        // Replies come and go unread, and the open Thread stays listed: keep the Threads section current.
        let threads = self.followed_items();
        self.sidebar.set(&Section::Threads, threads);
        // People's names keep arriving: keep a search's hits current.
        self.refresh_search();
        self.refresh_suggestions(&mut commands);
        self.refresh_details(&mut commands);
        self.refresh_reacting();
        self.reach_selected(&mut commands);
        self.snap_thread();
        commands
    }

    fn key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let pending = self.pending.take();
        match self.mode {
            Mode::Normal => self.normal_key(key, pending, commands),
            Mode::Insert => self.insert_key(key, pending, commands),
            Mode::Command(_) => self.command_key(key, commands),
            Mode::Search(_) => self.search_key(key, commands),
            Mode::Details(_) => self.details_key(key, commands),
            Mode::React(_) => self.react_key(key, commands),
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
                self.tell("type :q and press Enter to quit".to_owned());
                return;
            }
            (KeyCode::Char(':'), false) => {
                self.mode = Mode::Command(String::new());
                return;
            }
            (KeyCode::Char('\\'), false) => {
                match self.focus {
                    Focus::Sidebar if self.opened.is_some() => {
                        self.sidebar_shown = false;
                        self.focus = self.beside_sidebar();
                    }
                    Focus::Sidebar => self.tell("open a conversation first".to_owned()),
                    Focus::Timeline | Focus::Thread | Focus::Activity => self.focus_sidebar(),
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
                    self.focus = self.beside_sidebar();
                }
                match self.writing_place() {
                    Some(place) => {
                        self.draft(&place);
                        self.mode = Mode::Insert;
                    }
                    None => self.tell("open a conversation first".to_owned()),
                }
                return;
            }
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Sidebar => self.beside_sidebar(),
                    Focus::Timeline if self.open_thread.is_some() => Focus::Thread,
                    Focus::Timeline | Focus::Thread | Focus::Activity if self.sidebar_shown => {
                        Focus::Sidebar
                    }
                    Focus::Timeline | Focus::Thread | Focus::Activity => self.beside_sidebar(),
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
                    match self.sidebar.selected().cloned() {
                        Some(Row::Activity) => self.open_activity(commands),
                        Some(Row::Item {
                            item: Item::Conversation(conversation),
                            ..
                        }) => {
                            self.open(conversation, commands);
                            self.focus = Focus::Timeline;
                        }
                        Some(Row::Item {
                            item:
                                Item::Thread {
                                    conversation,
                                    parent,
                                },
                            ..
                        }) => {
                            self.open(conversation, commands);
                            self.open_thread(parent, commands);
                        }
                        Some(Row::Header(_)) => self.sidebar.fold(Fold::Toggle),
                        None => {}
                    }
                }
                _ => {}
            },
            Focus::Timeline => match key.code {
                KeyCode::Enter => self.open_selected_thread(commands),
                KeyCode::Char('U') => self.mark_unread_selected(commands),
                KeyCode::Char('+') => self.start_reacting(),
                KeyCode::Char('e') => self.edit_selected(),
                KeyCode::Char('K') => self.show_details(commands),
                KeyCode::Char('o') => self.open_selected_link(commands),
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => self.focus_sidebar(),
                _ => {
                    let Some(open) = self.opened.as_ref().and_then(Opened::conversation) else {
                        return;
                    };
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
                KeyCode::Char('U') => {
                    self.tell("only a conversation's messages can be marked unread".to_owned());
                }
                KeyCode::Char('+') => self.start_reacting(),
                KeyCode::Char('e') => self.edit_selected(),
                KeyCode::Char('K') => self.show_details(commands),
                KeyCode::Char('o') => self.open_selected_link(commands),
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => {
                    self.open_thread = None;
                    self.focus = Focus::Timeline;
                }
                _ => {
                    let (Some(open), Some(parent)) = (
                        self.opened.as_ref().and_then(Opened::conversation),
                        self.open_thread,
                    ) else {
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
            Focus::Activity => {
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => self.activity.select_by(1),
                    KeyCode::Char('k') | KeyCode::Up => self.activity.select_by(-1),
                    KeyCode::Char('g') | KeyCode::Home => self.activity.select_newest(),
                    KeyCode::Char('G') | KeyCode::End => self.activity.select_oldest(),
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                        return self.open_activity_item(commands);
                    }
                    KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => {
                        return self.focus_sidebar();
                    }
                    _ => return,
                }
                // At the oldest item loaded: load more.
                if self.activity.selected() + 1 >= self.activity.entries().len() {
                    for (key, request) in self.activity.load_older() {
                        commands.push(Command::Slack(key, request));
                    }
                }
            }
        }
    }

    fn insert_key(&mut self, key: KeyEvent, pending: Option<Prefix>, commands: &mut Vec<Command>) {
        let Some(place) = self.writing_place() else {
            self.mode = Mode::Normal;
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        // Shift+Enter needs the kitty keyboard protocol to tell it from Enter; Alt+Enter works without.
        let new_line = key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        if let Some(suggestions) = &mut self.suggestions {
            let count = suggestions.items.len();
            match (key.code, control) {
                (KeyCode::Down, _) | (KeyCode::Char('n'), true) => {
                    suggestions.selected = (suggestions.selected + 1) % count;
                    return;
                }
                (KeyCode::Up | KeyCode::BackTab, _) | (KeyCode::Char('p'), true) => {
                    suggestions.selected = (suggestions.selected + count - 1) % count;
                    return;
                }
                (KeyCode::Tab, _) | (KeyCode::Enter, _) if !new_line => {
                    let picked = suggestions.items[suggestions.selected].clone();
                    match picked.suggested {
                        Suggested::Mention(mentioned) => {
                            self.draft(&place).pick(mentioned, &picked.name);
                        }
                        Suggested::Command => self.draft(&place).pick_command(&picked.name),
                        Suggested::Emoji => {
                            self.draft(&place).pick_emoji(&picked.name);
                            if let Some(workspace) =
                                self.workspaces.get_mut(place.conversation.workspace)
                            {
                                workspace.emojis.note_use(&picked.name);
                            }
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
        let editing = self.editing.contains_key(&place);
        match (key.code, control) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => {
                if editing {
                    self.give_up_editing(&place);
                }
                self.mode = Mode::Normal;
            }
            (KeyCode::Char('x'), true) => self.pending = Some(Prefix::CtrlX),
            (KeyCode::Char('e'), true) if pending == Some(Prefix::CtrlX) => {
                let text = self.draft(&place).text();
                commands.push(Command::Edit { place, text });
            }
            (KeyCode::Enter, _) if new_line => self.draft(&place).insert_newline(),
            (KeyCode::Enter, _) if editing => self.save_edit(place, commands),
            (KeyCode::Enter, _) => self.send(place, commands),
            // As in Slack: in an empty composer, Up edits the user's newest Message there.
            (KeyCode::Up, false) if !editing && self.draft(&place).is_empty() => {
                if let Some(ts) = self.newest_of_mine(&place) {
                    self.start_editing(place, ts);
                }
            }
            _ => self.draft(&place).input(key),
        }
    }

    /// The Draft for `place`, empty if there was none.
    fn draft(&mut self, place: &Place) -> &mut Draft {
        self.drafts
            .entry(place.clone())
            .or_insert_with(|| Draft::new(place))
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
                    Err(reason) => self.tell(reason),
                }
            }
            _ => self.tell(format!("not a command: :{line}")),
        }
    }

    /// Puts the open Channel in the Virtual Workspace called `name` (in any case), creating it if there's none.
    ///
    /// # Errors
    ///
    /// Says why not: no Channel is open, or it's already there.
    fn add_to_virtual_workspace(&mut self, name: &str) -> Result<(), String> {
        let open = self
            .opened
            .as_ref()
            .and_then(Opened::conversation)
            .ok_or("open a channel first")?;
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
        let open = self
            .opened
            .as_ref()
            .and_then(Opened::conversation)
            .ok_or("open a channel first")?;
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
                    None => self.tell(format!("no conversation matches /{query}")),
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

    /// Offers what to complete the word being written in a Draft with, keeping the selected one selected; and, for an
    /// @-mention, asks Slack whom it knows by that name.
    fn refresh_suggestions(&mut self, commands: &mut Vec<Command>) {
        let place = self.writing_place().filter(|_| self.mode == Mode::Insert);
        let writing = place.and_then(|place| {
            let completing = self.drafts.get(&place)?.completing()?;
            Some((place, completing))
        });
        let Some((place, completing)) = writing else {
            self.suggestions = None;
            return;
        };
        let key = place.conversation.workspace;
        let Some(workspace) = self.workspaces.get_mut(key) else {
            self.suggestions = None;
            return;
        };
        let items: Vec<Suggestion> = match completing {
            Completing::Mention(name) => {
                workspace.find_people(key, &name, commands);
                workspace.suggest(&place, &name)
            }
            Completing::Command(name) => workspace.suggest_commands(&name),
            Completing::Emoji(name) => workspace
                .emojis
                .complete(&name, SUGGESTIONS)
                .into_iter()
                .map(|found| Suggestion {
                    suggested: Suggested::Emoji,
                    name: found.name,
                    detail: found.alias.to_owned(),
                    positions: found.positions,
                })
                .collect(),
        };
        let was_selected = self
            .suggestions
            .as_ref()
            .and_then(|old| old.items.get(old.selected));
        let selected = was_selected
            .and_then(|old| items.iter().position(|item| item.writes_as(old)))
            .unwrap_or(0);
        self.suggestions = (!items.is_empty()).then_some(Suggestions { items, selected });
    }

    /// Opens the Thread of the open Conversation's selected Message. Any Message can start a Thread: one without
    /// Replies yet opens empty, ready for one.
    fn open_selected_thread(&mut self, commands: &mut Vec<Command>) {
        if let Some((Place { thread: None, .. }, parent)) = self.selected_message() {
            self.open_thread(parent, commands);
        }
    }

    /// The focused pane's selected Message: where it's held, and when it was posted. `None` while the sidebar or
    /// Activity has focus, or the pane holds no Message.
    fn selected_message(&self) -> Option<(Place, Ts)> {
        let open = self.open_conversation()?;
        let workspace = self.workspaces.get(open.workspace)?;
        let (thread, timeline, pane) = match self.focus {
            Focus::Sidebar | Focus::Activity => return None,
            Focus::Timeline => (
                None,
                workspace.timelines.get(&open.id)?,
                &self.conversation_pane,
            ),
            Focus::Thread => {
                let parent = self.open_thread?;
                let thread = workspace.threads.get(&(open.id.clone(), parent))?;
                (Some(parent), thread, &self.thread_pane)
            }
        };
        let place = Place {
            conversation: open.clone(),
            thread,
        };
        Some((place, pane.selected(timeline)?))
    }

    /// The Message posted at `ts`, as `place` holds it, and its Workspace.
    pub(crate) fn message(&self, place: &Place, ts: Ts) -> Option<(&Workspace, &Message)> {
        let workspace = self.workspaces.get(place.conversation.workspace)?;
        let id = &place.conversation.id;
        let timeline = match place.thread {
            Some(parent) => workspace.threads.get(&(id.clone(), parent)),
            None => workspace.timelines.get(id),
        }?;
        Some((workspace, timeline.get(ts)?))
    }

    /// Shows the details of the focused pane's selected Message, asking Slack for everyone who reacted to it when
    /// its history named only some.
    fn show_details(&mut self, commands: &mut Vec<Command>) {
        let Some((place, ts)) = self.selected_message() else {
            return;
        };
        let Some((_, message)) = self.message(&place, ts) else {
            return;
        };
        if message
            .reactions
            .iter()
            .any(|reaction| reaction.unnamed() > 0)
        {
            let request = Request::Reactions {
                conversation: place.conversation.id.clone(),
                ts,
            };
            commands.push(Command::Slack(place.conversation.workspace, request));
        }
        self.mode = Mode::Details(Details {
            place,
            ts,
            selected: 0,
        });
    }

    /// Opens the link or file of the focused pane's selected Message when it has just one, and shows its details,
    /// to choose from, otherwise.
    fn open_selected_link(&mut self, commands: &mut Vec<Command>) {
        let Some((place, ts)) = self.selected_message() else {
            return;
        };
        let Some((workspace, message)) = self.message(&place, ts) else {
            return;
        };
        let links = workspace.links(&place.conversation.id, message);
        let mut own = links.iter().filter(|link| link.kind != LinkKind::Message);
        match (own.next(), own.next()) {
            (Some(only), None) => commands.push(Command::Open(only.url.clone())),
            _ => self.show_details(commands),
        }
    }

    fn details_key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let Mode::Details(details) = &self.mode else {
            return;
        };
        let (place, ts, selected) = (&details.place, details.ts, details.selected);
        let choices = self
            .message(place, ts)
            .map(|(workspace, message)| workspace.choices(&place.conversation.id, message))
            .unwrap_or_default();
        let take = |index: usize| {
            Some(match choices.get(index)? {
                Choice::Press(button) => {
                    let request = Request::Press {
                        conversation: place.conversation.id.clone(),
                        ts,
                        button: button.clone(),
                    };
                    Command::Slack(place.conversation.workspace, request)
                }
                Choice::Open(link) => Command::Open(link.url.clone()),
            })
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        // Taking a choice closes the details only when there's nothing else to choose.
        let only = choices.len() <= 1;
        let (command, close) = match (key.code, control) {
            // `refresh_details` keeps the selection on the last choice.
            (KeyCode::Down | KeyCode::Char('j'), false) | (KeyCode::Char('n'), true) => {
                return self.select_choice(selected + 1);
            }
            (KeyCode::Up | KeyCode::Char('k'), false) | (KeyCode::Char('p'), true) => {
                return self.select_choice(selected.saturating_sub(1));
            }
            (KeyCode::Enter | KeyCode::Char('o'), false) => (take(selected), only),
            (KeyCode::Char('y'), false) => match choices.get(selected) {
                Some(Choice::Open(link)) => (Some(Command::Copy(link.url.clone())), only),
                // A button has nothing to copy.
                Some(Choice::Press(_)) => return,
                None => (None, true),
            },
            (KeyCode::Char(digit @ '1'..='9'), false) => {
                let index = digit.to_digit(10).map_or(0, |digit| digit as usize - 1);
                let Some(command) = take(index) else { return };
                (Some(command), only)
            }
            (KeyCode::Esc | KeyCode::Char('q' | 'K'), false) | (KeyCode::Char('c'), true) => {
                (None, true)
            }
            _ => return,
        };
        commands.extend(command);
        if close {
            self.mode = Mode::Normal;
        }
    }

    fn select_choice(&mut self, index: usize) {
        if let Mode::Details(details) = &mut self.mode {
            details.selected = index;
        }
    }

    /// Keeps the details shown current: closes them once their Message is gone, keeps the selection on one of its
    /// choices, and asks for the names of the first people who reacted with each emoji.
    fn refresh_details(&mut self, commands: &mut Vec<Command>) {
        let Mode::Details(details) = &self.mode else {
            return;
        };
        let Some((workspace, message)) = self.message(&details.place, details.ts) else {
            self.mode = Mode::Normal;
            return;
        };
        let choices = workspace.choices(&details.place.conversation.id, message);
        let reactors: Vec<UserId> = message
            .reactions
            .iter()
            .flat_map(|reaction| reaction.users.iter().take(NAMED_REACTORS))
            .cloned()
            .collect();
        let key = details.place.conversation.workspace;
        let selected = details.selected.min(choices.len().saturating_sub(1));
        self.select_choice(selected);
        if let Some(workspace) = self.workspaces.get_mut(key) {
            workspace.want_users(key, reactors, commands);
        }
    }

    /// Edits the focused pane's selected Message, if it's the user's.
    fn edit_selected(&mut self) {
        let Some((place, ts)) = self.selected_message() else {
            return;
        };
        self.start_editing(place, ts);
    }

    /// The user's newest Message at `place`, if there's one.
    fn newest_of_mine(&self, place: &Place) -> Option<Ts> {
        let workspace = self.workspaces.get(place.conversation.workspace)?;
        let id = &place.conversation.id;
        let timeline = match place.thread {
            Some(parent) => workspace.threads.get(&(id.clone(), parent)),
            None => workspace.timelines.get(id),
        }?;
        timeline
            .messages()
            .rev()
            .find(|message| message.is_by(&workspace.me))
            .map(|message| message.ts)
    }

    /// Starts editing the Message at `ts`, held at `place`, in that Place's Draft, in Insert mode; what's being
    /// written there is set aside meanwhile. Says why not when the Message isn't the user's.
    fn start_editing(&mut self, place: Place, ts: Ts) {
        let Some((workspace, message)) = self.message(&place, ts) else {
            return;
        };
        if !message.is_by(&workspace.me) {
            self.tell("only your own messages can be edited".to_owned());
            return;
        }
        let draft = workspace.draft_of(&place, &message.content);
        self.write_edit(place, ts, draft);
        self.mode = Mode::Insert;
    }

    /// Puts `draft`, an edit of the Message at `ts`, in `place`'s composer, setting aside what's being written there.
    fn write_edit(&mut self, place: Place, ts: Ts, draft: Draft) {
        let before = self.drafts.insert(place.clone(), draft);
        // Editing another Message there drops that edit, but keeps what was set aside for it.
        let set_aside = match self.editing.remove(&place) {
            Some(earlier) => earlier.set_aside,
            None => before.filter(|draft| !draft.is_empty()),
        };
        self.editing.insert(place, Editing { ts, set_aside });
    }

    /// Gives up the edit at `place`, bringing back what was set aside for it.
    fn give_up_editing(&mut self, place: &Place) {
        let Some(editing) = self.editing.remove(place) else {
            return;
        };
        match editing.set_aside {
            Some(draft) => {
                self.drafts.insert(place.clone(), draft);
            }
            None => {
                self.drafts.remove(place);
            }
        }
    }

    /// Saves the edit at `place`, unless it's blank, and brings back what was set aside for it.
    fn save_edit(&mut self, place: Place, commands: &mut Vec<Command>) {
        let Some(written) = self.drafts.get_mut(&place).and_then(Draft::take) else {
            self.tell("an edit can't be empty: esc gives it up".to_owned());
            return;
        };
        let Some(editing) = self.editing.get(&place) else {
            return;
        };
        let ts = editing.ts;
        self.give_up_editing(&place);
        let request = Request::Edit {
            conversation: place.conversation.id.clone(),
            ts,
            text: written.parts(),
        };
        commands.push(Command::Slack(place.conversation.workspace, request));
        self.edits_sent.push((place, ts, written));
    }

    /// Whether a Message is being edited at `place`, in its Draft.
    pub(crate) fn is_editing(&self, place: &Place) -> bool {
        self.editing.contains_key(place)
    }

    /// Starts choosing an emoji to react to the focused pane's selected Message with.
    fn start_reacting(&mut self) {
        let Some((place, ts)) = self.selected_message() else {
            return;
        };
        self.mode = Mode::React(Reacting {
            place,
            ts,
            query: String::new(),
            choices: None,
        });
        self.refresh_reacting();
    }

    fn react_key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let Mode::React(reacting) = &mut self.mode else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = reacting
            .choices
            .as_ref()
            .map_or(0, |choices| choices.items.len());
        let select = |reacting: &mut Reacting, by: usize| {
            if let Some(choices) = &mut reacting.choices {
                choices.selected = (choices.selected + by) % count;
            }
        };
        match (key.code, control) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => self.mode = Mode::Normal,
            (KeyCode::Down, _) | (KeyCode::Char('n'), true) => select(reacting, 1),
            (KeyCode::Up | KeyCode::BackTab, _) | (KeyCode::Char('p'), true) => {
                select(reacting, count.saturating_sub(1));
            }
            (KeyCode::Enter | KeyCode::Tab, _) => {
                let picked = reacting
                    .choices
                    .as_ref()
                    .and_then(|choices| choices.items.get(choices.selected));
                if let Some(picked) = picked {
                    let (place, ts, name) =
                        (reacting.place.clone(), reacting.ts, picked.name.clone());
                    self.mode = Mode::Normal;
                    self.react(&place, ts, &name, commands);
                }
            }
            (KeyCode::Backspace, _) => {
                reacting.query.pop();
            }
            // Out of habit, emoji are often typed between colons: those aren't part of the name.
            (KeyCode::Char(':'), false) => {}
            (KeyCode::Char(c), false) => reacting.query.push(c),
            _ => {}
        }
    }

    /// Keeps what to react with current: what matches the query, the user's reactions among them saying so, the
    /// selected one selected. Stops choosing when the Message goes.
    fn refresh_reacting(&mut self) {
        let Mode::React(reacting) = &self.mode else {
            return;
        };
        let key = reacting.place.conversation.workspace;
        let Some((workspace, message)) = self.message(&reacting.place, reacting.ts) else {
            self.mode = Mode::Normal;
            return;
        };
        let mine: HashSet<String> = message
            .reactions
            .iter()
            .filter(|reaction| reaction.users.contains(&workspace.me))
            .map(|reaction| reaction.emoji.name.clone())
            .collect();
        let query = reacting.query.clone();
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return;
        };
        let items: Vec<Suggestion> = workspace
            .emojis
            .complete(&query, SUGGESTIONS)
            .into_iter()
            .map(|found| Suggestion {
                suggested: Suggested::Emoji,
                detail: if mine.contains(&found.name) {
                    "remove".to_owned()
                } else {
                    found.alias.to_owned()
                },
                name: found.name,
                positions: found.positions,
            })
            .collect();
        let Mode::React(reacting) = &mut self.mode else {
            return;
        };
        let was_selected = reacting
            .choices
            .as_ref()
            .and_then(|old| old.items.get(old.selected));
        let selected = was_selected
            .and_then(|old| items.iter().position(|item| item.writes_as(old)))
            .unwrap_or(0);
        reacting.choices = (!items.is_empty()).then_some(Suggestions { items, selected });
    }

    /// Reacts to the Message at `ts`, held at `place`, with the emoji `name`; or, when the user has reacted with it
    /// already, in any skin tone, takes that back. Shows it straight away, and undoes it should Slack refuse.
    fn react(&mut self, place: &Place, ts: Ts, name: &str, commands: &mut Vec<Command>) {
        let Some((workspace, message)) = self.message(place, ts) else {
            return;
        };
        let mine = message
            .reactions
            .iter()
            .find(|reaction| reaction.emoji.name == name && reaction.users.contains(&workspace.me))
            .map(|reaction| reaction.emoji.clone());
        let (emoji, change) = match mine {
            Some(emoji) => (emoji, ReactionChange::Removed),
            None => (Emoji::named(name), ReactionChange::Added),
        };
        let key = place.conversation.workspace;
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return;
        };
        let me = workspace.me.clone();
        for timeline in workspace.timelines_of(&place.conversation.id) {
            timeline.react(ts, &emoji, &me, change);
        }
        if change == ReactionChange::Added {
            workspace.emojis.note_use(name);
        }
        let request = Request::React {
            conversation: place.conversation.id.clone(),
            ts,
            emoji,
            change,
        };
        commands.push(Command::Slack(key, request));
    }

    /// Opens the Thread under `parent` in the open Conversation, in its own pane with focus, and asks for its
    /// Replies unless they're held or on their way. A Followed Thread opens on its first unread Reply, if any.
    fn open_thread(&mut self, parent: Ts, commands: &mut Vec<Command>) {
        let Some(open) = self.opened.as_ref().and_then(Opened::conversation) else {
            return;
        };
        self.open_thread = Some(parent);
        self.thread_shown = false;
        self.focus = Focus::Thread;
        let Some(workspace) = self.workspaces.get_mut(open.workspace) else {
            return;
        };
        let thread = (open.id.clone(), parent);
        // A Followed Thread opens on its first unread Reply, once loaded.
        let unread = workspace
            .followed
            .get(&thread)
            .map(|followed| &followed.unread)
            .filter(|unread| unread.has_unread())
            .map(Unread::read_up_to);
        self.thread_pane = unread.map_or_else(Pane::default, Pane::unread_after);
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
        let Some(written) = self.drafts.get_mut(&place).and_then(Draft::take) else {
            return;
        };
        let id = self.next_post;
        self.next_post += 1;
        if let Some((command, text)) = written.command() {
            let known = self
                .workspaces
                .get(place.conversation.workspace)
                .map(|workspace| &workspace.commands);
            // Until Slack lists them, let it say whether there's such a command.
            if known.is_some_and(|known| {
                !known.is_empty() && !known.iter().any(|offered| offered.name == command)
            }) {
                self.tell(format!(
                    "no {command} command here: start with a space to post it as text"
                ));
                self.draft(&place).put_back(written);
                return;
            }
            let request = Request::RunCommand {
                conversation: place.conversation.id.clone(),
                thread: place.thread,
                command,
                text,
                id,
            };
            commands.push(Command::Slack(place.conversation.workspace, request));
            self.following = Some(Following {
                from: place.conversation.clone(),
                at: now(),
            });
            self.commands_sent.push((id, place, written));
            return;
        }
        match place.thread {
            Some(_) => self.thread_pane.newest(),
            None => self.conversation_pane.newest(),
        }
        let request = Request::Post {
            conversation: place.conversation.id.clone(),
            thread: place.thread,
            text: written.parts(),
            id,
        };
        commands.push(Command::Slack(place.conversation.workspace, request));
        self.outbox.push(Outgoing { id, place, written });
    }

    /// Moves the open Conversation's Read Marker to its newest Message, if that's on screen: the window has focus,
    /// the Conversation's pane shows, and the newest Message is selected at the bottom.
    fn mark_seen(&mut self, commands: &mut Vec<Command>) {
        if !self.focused || !self.conversation_shown || !self.conversation_pane.at_newest() {
            return;
        }
        let Some(open) = self.opened.as_ref().and_then(Opened::conversation) else {
            return;
        };
        if self.kept_unread.as_ref() == Some(open) {
            return;
        }
        let Some(workspace) = self.workspaces.get_mut(open.workspace) else {
            return;
        };
        let newest = workspace.timelines.get(&open.id).and_then(Timeline::newest);
        let (Some(newest), Some(unread)) = (newest, workspace.unread.get_mut(&open.id)) else {
            return;
        };
        if newest > unread.read_up_to() {
            unread.mark_read(newest);
            let request = Request::Mark {
                conversation: open.id.clone(),
                up_to: newest,
            };
            commands.push(Command::Slack(open.workspace, request));
        }
    }

    /// Marks the Conversation pane's selected Message unread, and everything after it, keeping the Conversation
    /// unread while it's open.
    fn mark_unread_selected(&mut self, commands: &mut Vec<Command>) {
        let Some((place, ts)) = self.selected_message() else {
            return;
        };
        let conversation = place.conversation;
        let Some(unread) = self
            .workspaces
            .get_mut(conversation.workspace)
            .and_then(|workspace| workspace.unread.get_mut(&conversation.id))
        else {
            return;
        };
        unread.mark_unread_from(ts);
        let request = Request::Mark {
            conversation: conversation.id.clone(),
            up_to: unread.read_up_to(),
        };
        commands.push(Command::Slack(conversation.workspace, request));
        self.kept_unread = Some(conversation);
        self.tell("marked unread: it stays so until you open another conversation".to_owned());
    }

    /// Moves the open Thread's Read Marker to its newest Reply, if the user follows it and that's on screen: the
    /// window has focus, the Thread's pane shows, and the newest Reply is selected at the bottom.
    fn mark_thread_seen(&mut self, commands: &mut Vec<Command>) {
        if !self.focused || !self.thread_shown || !self.thread_pane.at_newest() {
            return;
        }
        let (Some(open), Some(parent)) = (
            self.opened.as_ref().and_then(Opened::conversation),
            self.open_thread,
        ) else {
            return;
        };
        let Some(workspace) = self.workspaces.get_mut(open.workspace) else {
            return;
        };
        let thread = (open.id.clone(), parent);
        let newest = workspace.threads.get(&thread).and_then(Timeline::newest);
        let (Some(newest), Some(followed)) = (newest, workspace.followed.get_mut(&thread)) else {
            return;
        };
        if newest > followed.unread.read_up_to() {
            followed.unread.mark_read(newest);
            let request = Request::MarkThread {
                conversation: open.id.clone(),
                parent,
                up_to: newest,
            };
            commands.push(Command::Slack(open.workspace, request));
        }
    }

    /// Moves focus to the sidebar, showing it if it's hidden.
    /// Notifies of the held Messages in the Workspace at `key` whose people all have names now, and keeps holding the
    /// others.
    fn notify_held(&mut self, key: WorkspaceKey, commands: &mut Vec<Command>) {
        let Some(workspace) = self.workspaces.get(key) else {
            return;
        };
        let several = self.workspaces.iter().count() > 1;
        self.held_notifications
            .retain(|(held, conversation, message)| {
                if *held != key || !workspace.knows_names_in(message) {
                    return true;
                }
                commands.push(workspace.notification(conversation, message, several));
                false
            });
    }

    /// Where focus goes beside the sidebar: to what's open there.
    fn beside_sidebar(&self) -> Focus {
        match self.opened {
            Some(Opened::Activity) => Focus::Activity,
            Some(Opened::Conversation(_)) | None => Focus::Timeline,
        }
    }

    /// Opens Activity beside the sidebar, and asks every Workspace for what's new in it.
    fn open_activity(&mut self, commands: &mut Vec<Command>) {
        self.opened = Some(Opened::Activity);
        self.open_thread = None;
        self.conversation_shown = false;
        self.thread_shown = false;
        self.focus = Focus::Activity;
        let ready: Vec<WorkspaceKey> = self
            .workspaces
            .iter()
            .filter(|(_, sign_in)| matches!(sign_in.phase, Phase::Ready(_)))
            .map(|(key, _)| key)
            .collect();
        for key in ready {
            self.refresh_activity(key, commands);
        }
    }

    /// Asks the Workspace at `key` for its newest Activity.
    fn refresh_activity(&mut self, key: WorkspaceKey, commands: &mut Vec<Command>) {
        if let Some(request) = self.activity.refresh(key) {
            commands.push(Command::Slack(key, request));
        }
    }

    /// Opens the selected Activity item's Message, as [`App::go_to`] does.
    fn open_activity_item(&mut self, commands: &mut Vec<Command>) {
        let Some(entry) = self.activity.selected_entry() else {
            return;
        };
        let key = entry.workspace;
        let mut at = entry.item.message.clone();
        at.thread = at.thread.or(match self.activity_message(key, &at) {
            Fetched::Got(message) => message.reply_to,
            Fetched::Unasked | Fetched::Fetching | Fetched::Unavailable => None,
        });
        self.go_to(key, at, commands);
    }

    /// Opens the Message at `at` in the Workspace at `key`, selected: in its Thread if it's a Reply, else in its
    /// Conversation, loading older history as far as [`JUMP_PAGES`] pages to reach it.
    fn go_to(&mut self, key: WorkspaceKey, at: MessageRef, commands: &mut Vec<Command>) {
        let conversation = ConversationRef {
            workspace: key,
            id: at.conversation,
        };
        if self.workspaces.conversation(&conversation).is_none() {
            self.tell("that conversation isn't in your sidebar".to_owned());
            return;
        }
        self.sidebar.select(&conversation);
        self.open(conversation, commands);
        match at.thread {
            Some(parent) => {
                self.open_thread(parent, commands);
                self.thread_pane.select(at.ts);
            }
            None => {
                self.focus = Focus::Timeline;
                self.conversation_pane.select(at.ts);
                self.reaching = Some(JUMP_PAGES);
            }
        }
    }

    /// The Message at `at` in the Workspace at `key`: as loaded in its Conversation or Thread, else as fetched for
    /// Activity.
    pub(crate) fn activity_message(&self, key: WorkspaceKey, at: &MessageRef) -> Fetched<'_> {
        let loaded = self.workspaces.get(key).and_then(|workspace| {
            let timeline = match at.thread {
                Some(parent) => workspace.threads.get(&(at.conversation.clone(), parent)),
                None => workspace.timelines.get(&at.conversation),
            };
            timeline?.get(at.ts)
        });
        loaded.map_or_else(|| self.activity.message(key, at), Fetched::Got)
    }

    /// Loads the open Conversation's older history while the Message it opened on is older than what's loaded, as
    /// far as [`JUMP_PAGES`] pages, then settles the selection on a Message loaded: the one it opened on, else the
    /// next one loaded.
    fn reach_selected(&mut self, commands: &mut Vec<Command>) {
        let Some(pages) = self.reaching else {
            return;
        };
        let Some(open) = self.opened.as_ref().and_then(Opened::conversation) else {
            self.reaching = None;
            return;
        };
        let Some(workspace) = self.workspaces.get(open.workspace) else {
            return;
        };
        let Some(timeline) = workspace.timelines.get(&open.id) else {
            // The first page is on its way.
            return;
        };
        if workspace.loading.contains(&open.id) {
            return;
        }
        let beyond = self
            .conversation_pane
            .cursor()
            .zip(timeline.oldest())
            .is_some_and(|(selected, oldest)| selected < oldest);
        if beyond && timeline.has_older() && pages > 0 {
            self.reaching = Some(pages - 1);
            self.load_older(commands);
        } else {
            self.reaching = None;
            self.conversation_pane.snap(timeline);
        }
    }

    /// Settles the open Thread's selection on a Reply it holds, once it's loaded: its first unread one, when it opened
    /// on that.
    fn snap_thread(&mut self) {
        let (Some(open), Some(parent)) = (
            self.opened.as_ref().and_then(Opened::conversation),
            self.open_thread,
        ) else {
            return;
        };
        let thread = self
            .workspaces
            .get(open.workspace)
            .and_then(|workspace| workspace.threads.get(&(open.id.clone(), parent)));
        if let Some(thread) = thread {
            self.thread_pane.snap(thread);
        }
    }

    fn focus_sidebar(&mut self) {
        self.sidebar_shown = true;
        self.focus = Focus::Sidebar;
    }

    /// Opens `conversation`: on its first unread Message when it has any, loading older history to reach it, else
    /// on its newest.
    fn open(&mut self, conversation: ConversationRef, commands: &mut Vec<Command>) {
        self.kept_unread = None;
        let unread = self
            .workspaces
            .unread(&conversation)
            .filter(|unread| unread.has_unread())
            .map(Unread::read_up_to);
        self.conversation_pane = unread.map_or_else(Pane::default, Pane::unread_after);
        self.reaching = unread.map(|_| JUMP_PAGES);
        self.conversation_shown = false;
        self.open_thread = None;
        self.thread_shown = false;
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
        self.opened = Some(Opened::Conversation(conversation));
    }

    /// Asks for the page before the oldest Message of the open Timeline, if there is one and it isn't on its way.
    fn load_older(&mut self, commands: &mut Vec<Command>) {
        let Some(open) = self.opened.as_ref().and_then(Opened::conversation) else {
            return;
        };
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

    /// Lists the sidebar's sections afresh: the Followed Threads with Replies to read, then each Virtual Workspace
    /// with its Channels, in the config's order, then each Workspace with its Conversations in no Virtual Workspace.
    fn list_sidebar(&mut self) {
        let mut in_virtual_workspaces = HashSet::new();
        let mut sections = vec![(Section::Threads, self.followed_items())];
        for virtual_workspace in &self.virtual_workspaces {
            let channels: Vec<ConversationRef> = virtual_workspace
                .channels
                .iter()
                .filter_map(|channel| self.workspaces.resolve(channel))
                .collect();
            in_virtual_workspaces.extend(channels.iter().cloned());
            let section = Section::VirtualWorkspace(virtual_workspace.name.clone());
            sections.push((
                section,
                channels.into_iter().map(Item::Conversation).collect(),
            ));
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
                    .map(Item::Conversation)
                    .collect(),
                Phase::SigningIn | Phase::Failed(_) => Vec::new(),
            };
            sections.push((Section::Workspace(key), conversations));
        }
        self.sidebar.list(sections);
    }

    /// The Followed Threads the Threads section lists: those with Replies to read, and the open one, so that it
    /// doesn't go as soon as it's read; the newest Reply first.
    fn followed_items(&self) -> Vec<Item> {
        let mut threads = Vec::new();
        for (key, sign_in) in self.workspaces.iter() {
            let Phase::Ready(workspace) = &sign_in.phase else {
                continue;
            };
            for ((id, parent), followed) in &workspace.followed {
                let conversation = ConversationRef {
                    workspace: key,
                    id: id.clone(),
                };
                let is_open = self.opened.as_ref().and_then(Opened::conversation)
                    == Some(&conversation)
                    && self.open_thread == Some(*parent);
                if (followed.unread.has_unread() || is_open) && workspace.conversation(id).is_some()
                {
                    threads.push((followed.unread.latest(), key.0, *parent, conversation));
                }
            }
        }
        // Newest first; the rest only makes the order the same every time.
        threads.sort_by_cached_key(|(latest, index, parent, conversation)| {
            (
                Reverse(*latest),
                *index,
                *parent,
                conversation.id.to_string(),
            )
        });
        threads
            .into_iter()
            .map(|(_, _, parent, conversation)| Item::Thread {
                conversation,
                parent,
            })
            .collect()
    }

    fn slack(&mut self, key: WorkspaceKey, update: Update, commands: &mut Vec<Command>) {
        match update {
            Update::Ready(snapshot) => self.ready(key, *snapshot, commands),
            Update::SignInFailed(error) => {
                self.workspaces
                    .set_phase(key, Phase::Failed(error.to_string()));
            }
            Update::Commands(offered) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.commands = offered;
                }
            }
            Update::RanCommand { id, result } => {
                let Some(at) = self.commands_sent.iter().position(|(sent, ..)| *sent == id) else {
                    return;
                };
                let (_, place, written) = self.commands_sent.remove(at);
                match result {
                    Ok(Some(reply)) => {
                        if let Some(workspace) = self.workspaces.get(key) {
                            self.tell(workspace.gist(&reply));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if self
                            .following
                            .as_ref()
                            .is_some_and(|following| following.from == place.conversation)
                        {
                            self.following = None;
                        }
                        let command = written
                            .command()
                            .map(|(command, _)| command)
                            .unwrap_or_default();
                        self.tell(format!(
                            "couldn't run {command}, back in the draft: {}",
                            describe(&error)
                        ));
                        self.draft(&place).put_back(written);
                    }
                }
            }
            Update::Edited {
                conversation,
                ts,
                result,
            } => {
                let sent = self.edits_sent.iter().position(|(place, sent, _)| {
                    place.conversation.workspace == key
                        && place.conversation.id == conversation
                        && *sent == ts
                });
                let Some(sent) = sent.map(|at| self.edits_sent.remove(at)) else {
                    return;
                };
                match result {
                    Ok(content) => {
                        if let Some(workspace) = self.workspaces.get_mut(key) {
                            for timeline in workspace.timelines_of(&conversation) {
                                timeline.revise(ts, content.clone());
                            }
                        }
                    }
                    Err(error) => {
                        let (place, ts, written) = sent;
                        self.tell(format!(
                            "not edited, back in the composer: {}",
                            describe(&error)
                        ));
                        let mut draft = Draft::new(&place);
                        draft.put_back(written);
                        self.write_edit(place, ts, draft);
                    }
                }
            }
            Update::Reacted {
                conversation,
                ts,
                emoji,
                change,
                result: Err(error),
            } => {
                // Undo what was shown.
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    let me = workspace.me.clone();
                    let undo = match change {
                        ReactionChange::Added => ReactionChange::Removed,
                        ReactionChange::Removed => ReactionChange::Added,
                    };
                    for timeline in workspace.timelines_of(&conversation) {
                        timeline.react(ts, &emoji, &me, undo);
                    }
                }
                let what = match change {
                    ReactionChange::Added => "react with",
                    ReactionChange::Removed => "take back",
                };
                self.tell(format!(
                    "couldn't {what} :{}:: {}",
                    emoji.name,
                    describe(&error)
                ));
            }
            Update::Reacted { result: Ok(()), .. } => {}
            Update::Pressed {
                button,
                result: Err(error),
                ..
            } => {
                self.tell(format!(
                    "couldn't press {}: {}",
                    button.label,
                    describe(&error)
                ));
            }
            // What pressing it did arrives live.
            Update::Pressed { result: Ok(()), .. } => {}
            Update::Activity { cursor, result } => {
                let next = match result {
                    Ok(page) => {
                        let people: Vec<UserId> =
                            page.items
                                .iter()
                                .filter_map(|item| match &item.happened {
                                    Happened::Mention { author, .. }
                                    | Happened::Added { author } => Some(author.clone()),
                                    Happened::Reaction { user, .. } => Some(user.clone()),
                                    Happened::Replies { .. } | Happened::Dm => None,
                                })
                                .collect();
                        if let Some(workspace) = self.workspaces.get_mut(key) {
                            workspace.want_users(key, people, commands);
                        }
                        self.activity.received(key, cursor, page)
                    }
                    Err(error) => {
                        tracing::warn!(%error, "couldn't load activity");
                        self.activity.failed(key)
                    }
                };
                commands.extend(next.map(|request| Command::Slack(key, request)));
            }
            Update::Message {
                conversation,
                ts,
                result,
            } => {
                let message = result.unwrap_or_else(|error| {
                    tracing::warn!(%error, "couldn't fetch a message");
                    None
                });
                if let Some(message) = &message
                    && let Some(workspace) = self.workspaces.get_mut(key)
                {
                    workspace.want_names_in(key, [message], commands);
                }
                self.activity.fetched(key, conversation, ts, message);
            }
            Update::Emoji(EmojiList { custom, standard }) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.emojis.set(custom, standard);
                }
            }
            Update::EmojiUse(used) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.emojis.set_used(used);
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
                        workspace.want_names_in(key, &page.messages, commands);
                        let timeline = workspace.timelines.entry(conversation).or_default();
                        timeline.add_history(page.messages, page.has_more);
                    }
                    Err(error) => self.tell(describe(&error)),
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
                        workspace.want_names_in(key, &messages, commands);
                        workspace
                            .threads
                            .entry(thread)
                            .or_insert_with(|| Timeline::thread(parent))
                            .add_history(messages, false);
                    }
                    Err(error) => self.tell(describe(&error)),
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
                        match message.reply_to {
                            Some(parent) => workspace.count_reply(&conversation, parent, &message),
                            None => {
                                if let Some(unread) = workspace.unread.get_mut(&conversation) {
                                    unread.mark_read(message.ts);
                                }
                            }
                        }
                        // It may @-mention people found by name, whose names are known only to the search.
                        workspace.want_names_in(key, [&message], commands);
                        workspace.add(&conversation, message);
                    }
                    Err(error) => {
                        self.tell(format!("not sent, back in the draft: {}", describe(&error)));
                        self.draft(&outgoing.place).put_back(outgoing.written);
                    }
                }
            }
            Update::User(user) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.users.insert(user.id.clone(), user);
                }
                self.notify_held(key, commands);
            }
            Update::Bot(bot) => {
                if let Some(workspace) = self.workspaces.get_mut(key) {
                    workspace.bots.insert(bot.id.clone(), bot);
                }
                self.notify_held(key, commands);
            }
            Update::Reactions {
                conversation,
                ts,
                result,
            } => match result {
                Ok(reactions) => {
                    if let Some(workspace) = self.workspaces.get_mut(key) {
                        for timeline in workspace.timelines_of(&conversation) {
                            timeline.set_reactions(ts, reactions.clone());
                        }
                    }
                }
                Err(error) => {
                    self.tell(format!(
                        "couldn't list everyone who reacted: {}",
                        describe(&error)
                    ));
                }
            },
            Update::FoundPeople { query, result } => {
                let Some(workspace) = self.workspaces.get_mut(key) else {
                    return;
                };
                workspace.finding = None;
                // Counted as finding no one, so as not to ask again with every key.
                let people = result.unwrap_or_else(|error| {
                    tracing::warn!(%error, "couldn't search for people");
                    Vec::new()
                });
                workspace.found.insert(query, people);
            }
            Update::Downloaded { url, result } => {
                let Some(fit) = self.pictures.awaited(&url) else {
                    return;
                };
                match result {
                    Ok(bytes) => commands.push(Command::Prepare { url, bytes, fit }),
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
        let partners: Vec<UserId> = workspace.dm_partners().cloned().collect();
        workspace.want_users(key, partners, commands);
        workspace.follow_presence(key, commands);
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
        // Each is logged; the status line has room for one.
        let mut missed = None;
        for (name, channel) in missing {
            tracing::warn!(virtual_workspace = %name, %channel, "not among the user's channels");
            missed = Some(format!(
                "{name} lists {channel}, which isn't among your channels"
            ));
        }
        if let Some(missed) = missed {
            self.tell(missed);
        }

        self.list_sidebar();
        self.refresh_activity(key, commands);
        // Put the cursor on the first row with something unread, without opening it: opening marks it read, which
        // the user should choose.
        let workspaces = &self.workspaces;
        self.sidebar
            .suggest(|row| row.item().is_some_and(|item| workspaces.has_unread(item)));
    }

    fn live(&mut self, key: WorkspaceKey, event: LiveEvent, commands: &mut Vec<Command>) {
        let now = self.animation_time();
        let Some(workspace) = self.workspaces.get_mut(key) else {
            return;
        };
        let place = |conversation: &ConversationId, thread| Place {
            conversation: ConversationRef {
                workspace: key,
                id: conversation.clone(),
            },
            thread,
        };
        let mut joined = false;
        // Where a slash command the user ran posted for them.
        let mut landed = None;
        // Whether it may add to the user's Activity, or change it.
        let mut touches_activity = false;
        // A Message to notify the user of.
        let mut notify = None;
        match event {
            LiveEvent::Joined {
                conversation,
                unread,
            } => {
                let partner = match &conversation.kind {
                    ConversationKind::Dm { with } => Some(with.clone()),
                    ConversationKind::Channel { .. } | ConversationKind::GroupDm => None,
                };
                joined = workspace.join(conversation, unread);
                if joined {
                    workspace.want_users(key, partner, commands);
                    workspace.follow_presence(key, commands);
                }
                // Someone added the user to a Channel, or wrote to them.
                touches_activity = joined;
            }
            LiveEvent::Posted {
                conversation,
                message,
            } => {
                if message.is_by(&workspace.me)
                    && self
                        .following
                        .as_ref()
                        .is_some_and(|following| following.led_to(key, &conversation, message.ts))
                {
                    landed = Some(MessageRef {
                        conversation: conversation.clone(),
                        ts: message.ts,
                        thread: message.reply_to,
                    });
                }
                workspace.want_names_in(key, [&message], commands);
                if let Author::User(author) = &message.author {
                    self.typing
                        .posted(&place(&conversation, message.reply_to), author);
                }
                let kind = workspace.conversation(&conversation).map(|c| &c.kind);
                let is_dm = matches!(kind, Some(ConversationKind::Dm { .. }));
                let in_followed = message.reply_to.is_some_and(|parent| {
                    workspace
                        .followed
                        .contains_key(&(conversation.clone(), parent))
                });
                let to_everyone = message
                    .content
                    .inlines()
                    .any(|inline| matches!(inline, Inline::Broadcast(_)));
                let by_someone_else = !message.is_by(&workspace.me);
                let in_dm = is_dm || matches!(kind, Some(ConversationKind::GroupDm));
                let mentions_me = message.mentions(&workspace.me);
                touches_activity =
                    by_someone_else && (in_dm || in_followed || to_everyone || mentions_me);
                // As Slack notifies by default: of DMs, Mentions and Replies in Followed Threads.
                if by_someone_else && (in_dm || in_followed || mentions_me) && !self.focused {
                    notify = Some((conversation.clone(), message.clone()));
                }
                if let Some(parent) = message.reply_to {
                    workspace.count_reply(&conversation, parent, &message);
                } else if !message.is_by(&workspace.me)
                    && let Some(unread) = workspace.unread.get_mut(&conversation)
                {
                    // Every DM Message counts as a Mention, as Slack's badges do.
                    unread.record(message.ts, is_dm || message.mentions(&workspace.me));
                }
                workspace.add(&conversation, message);
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
                user,
                author,
                change,
            } => {
                touches_activity = change == ReactionChange::Added
                    && user != workspace.me
                    && author.as_ref() == Some(&workspace.me);
                for timeline in workspace.timelines_of(&conversation) {
                    timeline.react(ts, &emoji, &user, change);
                }
            }
            LiveEvent::Presence { people, presence } => {
                for person in people {
                    workspace.presence.insert(person, presence);
                }
            }
            LiveEvent::Typing {
                conversation,
                thread,
                user,
            } => {
                // The user's own, from another client, is no news to them.
                if user != workspace.me {
                    workspace.want_users(key, [user.clone()], commands);
                    self.typing.heard(place(&conversation, thread), user, now);
                }
            }
            LiveEvent::Marked {
                conversation,
                up_to,
            } => {
                // Slack may now see the user's Activity there as read.
                touches_activity = self.activity.has_unread_in(key, &conversation);
                if let Some(unread) = workspace.unread.get_mut(&conversation) {
                    unread.mark_read(up_to);
                }
            }
            LiveEvent::ThreadFollowed {
                conversation,
                parent,
                read_up_to,
            } => workspace.follow(conversation, parent, read_up_to),
            LiveEvent::ThreadUnfollowed {
                conversation,
                parent,
            } => {
                workspace.followed.remove(&(conversation, parent));
            }
            LiveEvent::ThreadMarked {
                conversation,
                parent,
                up_to,
            } => {
                touches_activity = self.activity.has_unread_in(key, &conversation);
                if let Some(followed) = workspace.followed.get_mut(&(conversation, parent)) {
                    followed.unread.mark_read(up_to);
                }
            }
        }
        if joined {
            self.list_sidebar();
        }
        if let Some(at) = landed {
            self.following = None;
            let there = ConversationRef {
                workspace: key,
                id: at.conversation.clone(),
            };
            if self.open_conversation() != Some(&there) {
                self.go_to(key, at, commands);
            }
        }
        if touches_activity {
            self.refresh_activity(key, commands);
        }
        if let Some((conversation, message)) = notify {
            self.held_notifications.push((key, conversation, message));
            if self.held_notifications.len() > HELD_NOTIFICATIONS {
                self.held_notifications.remove(0);
            }
            self.notify_held(key, commands);
        }
    }
}

/// Something worth telling the user, usually a problem, on the status line for a while.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) text: String,
    /// When it stops showing, as animation time.
    pub(crate) until: Duration,
}

/// A slash command just run, which may post for the user elsewhere, as `/dm` does.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Following {
    /// Where it was run.
    from: ConversationRef,
    /// When, by this machine's clock.
    at: Ts,
}

impl Following {
    /// Whether the user's Message at `ts`, in `conversation` of the Workspace at `key`, is the command posting for
    /// them elsewhere: in another Conversation of its Workspace, soon after it ran.
    fn led_to(&self, key: WorkspaceKey, conversation: &ConversationId, ts: Ts) -> bool {
        let at = self.at.as_micros();
        let soon = at.saturating_sub(CLOCK_SKEW)..=at.saturating_add(COMMAND_POSTS_WITHIN);
        key == self.from.workspace
            && *conversation != self.from.id
            && soon.contains(&ts.as_micros())
    }
}

/// The time now, by this machine's clock, as Slack times Messages.
fn now() -> Ts {
    let micros = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_or(0, |since| since.as_micros());
    Ts::from_micros(u64::try_from(micros).unwrap_or(u64::MAX))
}

impl Workspace {
    /// The Workspace `snapshot` shows, with its Conversations in sidebar order: Channels by name, then DMs with the
    /// most recent first, as Slack's sidebar does by default.
    fn new(snapshot: Snapshot) -> Self {
        let followed = snapshot
            .followed_threads
            .into_iter()
            .map(|thread| {
                let key = (thread.conversation, thread.parent.ts);
                let followed = Followed {
                    unread: thread.unread,
                    parent: Some(thread.parent),
                };
                (key, followed)
            })
            .collect();
        let mut conversations = snapshot.conversations;
        conversations.sort_by(|(a, a_unread), (b, b_unread)| {
            is_dm(a).cmp(&is_dm(b)).then_with(|| {
                if is_dm(a) {
                    b_unread.latest().cmp(&a_unread.latest())
                } else {
                    a.name.cmp(&b.name)
                }
            })
        });
        let unread = conversations
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
            bots: HashMap::new(),
            requested_bots: HashSet::new(),
            presence: HashMap::new(),
            presence_followed: Vec::new(),
            emojis: Emojis::default(),
            permalinks: snapshot.permalinks,
            conversations: conversations.into_iter().map(|(c, _)| c).collect(),
            unread,
            timelines: HashMap::new(),
            threads: HashMap::new(),
            loading: HashSet::new(),
            loading_threads: HashSet::new(),
            followed,
            commands: Vec::new(),
            found: HashMap::new(),
            finding: None,
        }
    }

    /// Counts `reply`, posted under `parent` in `conversation`, on its Followed Thread. Replying to a Thread follows
    /// it, as in Slack; so does being mentioned in one here, so that the Mention isn't missed.
    fn count_reply(&mut self, conversation: &ConversationId, parent: Ts, reply: &Message) {
        let thread = (conversation.clone(), parent);
        let mine = reply.is_by(&self.me);
        let mentions_me = reply.mentions(&self.me);
        if !(self.followed.contains_key(&thread) || mine || mentions_me) {
            return;
        }
        let held = self.held_parent(conversation, parent);
        let followed = self.followed.entry(thread).or_insert_with(|| Followed {
            unread: Unread::default(),
            parent: held,
        });
        if mine {
            followed.unread.mark_read(reply.ts);
        } else {
            followed.unread.record(reply.ts, mentions_me);
        }
    }

    /// Follows the Thread under `parent` in `conversation`, read up to `read_up_to`. Its newest Reply is known only if
    /// its parent is held.
    fn follow(&mut self, conversation: ConversationId, parent: Ts, read_up_to: Ts) {
        let held = self.held_parent(&conversation, parent);
        let latest = held
            .as_ref()
            .and_then(|parent| parent.thread)
            .map_or(read_up_to, |summary| summary.latest.max(read_up_to));
        let followed = self
            .followed
            .entry((conversation, parent))
            .or_insert_with(|| Followed {
                unread: Unread::counted(read_up_to, latest, latest > read_up_to, 0),
                parent: held,
            });
        followed.unread.mark_read(read_up_to);
    }

    /// The Message at `parent` in `conversation`, if it's held.
    fn held_parent(&self, conversation: &ConversationId, parent: Ts) -> Option<Message> {
        self.timelines.get(conversation)?.get(parent).cloned()
    }

    /// Lists `conversation`, which just appeared, with its `unread`: a Channel among the Channels by name, a DM
    /// first among the DMs, as the most recently active. Returns whether it wasn't listed yet.
    fn join(&mut self, conversation: Conversation, unread: Unread) -> bool {
        if self.conversation(&conversation.id).is_some() {
            return false;
        }
        let at = if is_dm(&conversation) {
            self.conversations.iter().position(is_dm)
        } else {
            self.conversations
                .iter()
                .position(|listed| is_dm(listed) || listed.name > conversation.name)
        };
        self.unread.insert(conversation.id.clone(), unread);
        self.conversations
            .insert(at.unwrap_or(self.conversations.len()), conversation);
        true
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

    /// What `author` is called, as far as known: a bot not looked up yet is just "bot".
    pub(crate) fn author_name<'a>(&'a self, author: &'a Author) -> &'a str {
        match author {
            Author::User(id) => self.user_name(id),
            Author::Bot { name, .. } => name,
            Author::UnnamedBot(id) => self.bots.get(id).map_or("bot", |bot| bot.name.as_str()),
        }
    }

    /// Where `message`, posted in `conversation`, leads: the web pages its text links to, each once, then its files,
    /// then the Message itself. Only `http` and `https` links are among them, so that none starts a program the user
    /// didn't mean to.
    pub(crate) fn links(&self, conversation: &ConversationId, message: &Message) -> Vec<Link> {
        let mut links: Vec<Link> = Vec::new();
        for inline in message.content.inlines() {
            if let Inline::Link { url, label } = inline
                && !links.iter().any(|link| link.url == *url)
            {
                links.push(Link {
                    kind: LinkKind::Web,
                    label: label.clone().unwrap_or_else(|| url.clone()),
                    url: url.clone(),
                });
            }
        }
        links.extend(message.files.iter().filter_map(|file| {
            Some(Link {
                kind: LinkKind::File,
                label: file.name.clone(),
                url: file.link.clone()?,
            })
        }));
        // Slack doesn't keep an ephemeral Message, so there's no page for it.
        if !message.ephemeral {
            // The link to a Thread's parent or Reply opens the Thread.
            let thread = message.reply_to.or(message.thread.map(|_| message.ts));
            links.push(Link {
                kind: LinkKind::Message,
                label: "this message in Slack".to_owned(),
                url: self.permalinks.message(conversation, message.ts, thread),
            });
        }
        links.retain(|link| is_web(&link.url));
        links
    }

    /// What `message`'s details offer, in order: the buttons an app put in it, then its [`Workspace::links`].
    pub(crate) fn choices(&self, conversation: &ConversationId, message: &Message) -> Vec<Choice> {
        let buttons = message.content.blocks.iter().flat_map(|block| match block {
            Block::Actions(buttons) => buttons.as_slice(),
            Block::Paragraph(_)
            | Block::Quote(_)
            | Block::Preformatted(_)
            | Block::List { .. }
            | Block::Image { .. } => &[],
        });
        buttons
            .cloned()
            .map(Choice::Press)
            .chain(
                self.links(conversation, message)
                    .into_iter()
                    .map(Choice::Open),
            )
            .collect()
    }

    /// Where `author`'s avatar image is, as far as known.
    pub(crate) fn avatar<'a>(&'a self, author: &'a Author) -> Option<&'a str> {
        match author {
            Author::User(id) => self.users.get(id)?.avatar.as_deref(),
            Author::Bot { avatar, .. } => avatar.as_deref(),
            Author::UnnamedBot(id) => self.bots.get(id)?.avatar.as_deref(),
        }
    }

    /// A Draft at `place` holding `content` as one would write it: styles as `mrkdwn` markers, emoji by name, and
    /// people, broadcasts, user groups, Channels and labelled links as labels standing for them. Images and buttons
    /// go: they aren't text.
    fn draft_of(&self, place: &Place, content: &RichText) -> Draft {
        let mut draft = Draft::new(place);
        for (index, block) in content.blocks.iter().enumerate() {
            if index > 0 {
                draft.push_text("\n");
            }
            match block {
                Block::Paragraph(inlines) => self.write_inlines(&mut draft, inlines, ""),
                Block::Quote(inlines) => {
                    draft.push_text("> ");
                    self.write_inlines(&mut draft, inlines, "> ");
                }
                Block::Preformatted(text) => draft.push_text(&format!("```\n{text}\n```")),
                Block::List { ordered, items } => {
                    for (number, item) in items.iter().enumerate() {
                        if number > 0 {
                            draft.push_text("\n");
                        }
                        let marker = if *ordered {
                            format!("{}. ", number + 1)
                        } else {
                            "• ".to_owned()
                        };
                        draft.push_text(&marker);
                        self.write_inlines(&mut draft, item, "");
                    }
                }
                Block::Image { .. } | Block::Actions(_) => {}
            }
        }
        draft
    }

    /// Writes `inlines` at the end of `draft`, starting each line after the first with `line_start`.
    fn write_inlines(&self, draft: &mut Draft, inlines: &[Inline], line_start: &str) {
        for inline in inlines {
            match inline {
                Inline::Text { text, style } => {
                    let styled = styled(text, *style);
                    draft.push_text(&styled.replace('\n', &format!("\n{line_start}")));
                }
                Inline::Emoji(emoji) => {
                    let tone = emoji
                        .skin_tone
                        .map(|tone| format!(":skin-tone-{tone}:"))
                        .unwrap_or_default();
                    draft.push_text(&format!(":{}:{tone}", emoji.name));
                }
                Inline::User(id) => {
                    let label = format!("@{}", self.user_name(id));
                    draft.push_reference(&label, Reference::Mention(Mentioned::Person(id.clone())));
                }
                Inline::Broadcast(broadcast) => {
                    let label = match broadcast {
                        Broadcast::Here => "@here",
                        Broadcast::Channel => "@channel",
                        Broadcast::Everyone => "@everyone",
                    };
                    draft.push_reference(
                        label,
                        Reference::Mention(Mentioned::Broadcast(*broadcast)),
                    );
                }
                Inline::UserGroup(id) => {
                    let group = Reference::Mention(Mentioned::UserGroup(id.clone()));
                    draft.push_reference("@group", group);
                }
                Inline::Channel(id) => {
                    let name = self
                        .conversation(id)
                        .map_or(id.as_str(), |found| found.name.as_str());
                    draft.push_reference(&format!("#{name}"), Reference::Channel(id.clone()));
                }
                Inline::Link { url, label } => match label {
                    Some(label) if !label.is_empty() && label != url => {
                        draft.push_reference(label, Reference::Link(url.clone()));
                    }
                    _ => draft.push_text(url),
                },
            }
        }
    }

    /// `text` as one line of plain words: people, Channels and emoji by name, links by their label.
    pub(crate) fn gist(&self, text: &RichText) -> String {
        let mut gist = String::new();
        for inline in text.inlines() {
            match inline {
                Inline::Text { text, .. } => gist.push_str(text),
                Inline::Emoji(emoji) => gist.push_str(&format!(":{}:", emoji.name)),
                Inline::User(id) => gist.push_str(&format!("@{}", self.user_name(id))),
                Inline::Channel(id) => {
                    let name = self
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

    /// A notification of `message`, in `conversation`: who wrote it and where, and its gist. With `several`
    /// Workspaces signed in to, it names this one too.
    fn notification(
        &self,
        conversation: &ConversationId,
        message: &Message,
        several: bool,
    ) -> Command {
        let author = self.author_name(&message.author).to_owned();
        let place = self
            .conversation(conversation)
            .and_then(|found| match &found.kind {
                ConversationKind::Channel { .. } => Some(format!("#{}", found.name)),
                ConversationKind::GroupDm => Some(self.conversation_name(found).to_owned()),
                ConversationKind::Dm { .. } => None,
            });
        let mut title = match (message.reply_to.is_some(), place) {
            (false, Some(place)) => format!("{author} in {place}"),
            (true, Some(place)) => format!("{author} in a thread in {place}"),
            (false, None) => author,
            (true, None) => format!("{author} in a thread"),
        };
        if several {
            title.push_str(&format!(" · {}", self.name));
        }
        let mut body = self.gist(&message.content);
        if body.is_empty() {
            let files: Vec<&str> = message
                .files
                .iter()
                .map(|file| file.name.as_str())
                .collect();
            body = files.join(", ");
        }
        Command::Notify { title, body }
    }

    /// What to call `conversation`: a DM after the other person, anything else by its name.
    pub(crate) fn conversation_name<'a>(&'a self, conversation: &'a Conversation) -> &'a str {
        match &conversation.kind {
            ConversationKind::Dm { with } => self.user_name(with),
            ConversationKind::Channel { .. } | ConversationKind::GroupDm => &conversation.name,
        }
    }

    /// The slash commands to offer for `name`, being written after a `/`, best first: those whose names it starts, then
    /// those it starts a later word of, each by name.
    fn suggest_commands(&self, name: &str) -> Vec<Suggestion> {
        let mut found: Vec<Suggestion> = self
            .commands
            .iter()
            .filter_map(|command| {
                let bare = command.name.strip_prefix('/').unwrap_or(&command.name);
                let positions = search::word_start(bare, name)?;
                let detail = [command.usage.as_str(), command.description.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("  ");
                Some(Suggestion {
                    suggested: Suggested::Command,
                    name: command.name.clone(),
                    detail,
                    // Past the `/`.
                    positions: positions.into_iter().map(|at| at + 1).collect(),
                })
            })
            .collect();
        // Stable: those equally good stay by name.
        found.sort_by_key(|suggestion| suggestion.positions.first().is_some_and(|&at| at > 1));
        found.truncate(SUGGESTIONS);
        found
    }

    /// Asks Slack, through this Workspace at `key`, whom it knows by `name`, unless it was asked already. One search
    /// at a time: the next waits for the answer, by which time the name has likely grown.
    fn find_people(&mut self, key: WorkspaceKey, name: &str, commands: &mut Vec<Command>) {
        let query = name.to_lowercase();
        if query.is_empty() || self.finding.is_some() || self.found.contains_key(&query) {
            return;
        }
        self.finding = Some(query.clone());
        commands.push(Command::Slack(key, Request::FindPeople { query }));
    }

    /// Whom to offer for an @-mention of `name` written at `place`, best first. First, those known here whose names
    /// `name` starts a word of, those it starts first: the people who wrote there most recently, then in a Channel
    /// its broadcasts, then everyone else. After them, whom Slack found by that name, which may be more loosely.
    fn suggest(&self, place: &Place, name: &str) -> Vec<Suggestion> {
        let conversation = &place.conversation.id;
        let timeline = match place.thread {
            Some(parent) => self.threads.get(&(conversation.clone(), parent)),
            None => self.timelines.get(conversation),
        };
        let writers = timeline
            .into_iter()
            .flat_map(|timeline| timeline.messages().rev())
            .filter_map(|message| match &message.author {
                Author::User(id) => Some(id),
                Author::Bot { .. } | Author::UnnamedBot(_) => None,
            });
        let mut everyone: Vec<&User> = self.users.values().collect();
        everyone.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let mut seen = HashSet::from([&self.me]);
        let mut known = |id: &UserId| -> Option<(Mentioned, String)> {
            let user = self.users.get(id)?;
            seen.insert(&user.id)
                .then(|| (Mentioned::Person(user.id.clone()), user.name.clone()))
        };
        let mut candidates: Vec<(Mentioned, String)> = writers.filter_map(&mut known).collect();
        let in_channel = self
            .conversation(conversation)
            .is_some_and(|c| matches!(c.kind, ConversationKind::Channel { .. }));
        if in_channel {
            candidates.extend([
                (Mentioned::Broadcast(Broadcast::Here), "here".to_owned()),
                (
                    Mentioned::Broadcast(Broadcast::Channel),
                    "channel".to_owned(),
                ),
            ]);
        }
        candidates.extend(everyone.iter().filter_map(|user| known(&user.id)));
        let mut suggestions: Vec<Suggestion> = candidates
            .into_iter()
            .filter_map(|(mentioned, candidate)| {
                let positions = search::word_start(&candidate, name)?;
                Some(Suggestion {
                    suggested: Suggested::Mention(mentioned),
                    name: candidate,
                    detail: String::new(),
                    positions,
                })
            })
            .collect();
        // Stable: those equally good keep their order.
        suggestions.sort_by_key(|s| s.positions.first().is_some_and(|&at| at > 0));
        for person in self.found.get(&name.to_lowercase()).into_iter().flatten() {
            if person.id == self.me {
                continue;
            }
            let detail = if person.full_name.eq_ignore_ascii_case(&person.name) {
                String::new()
            } else {
                person.full_name.clone()
            };
            let suggested = Suggested::Mention(Mentioned::Person(person.id.clone()));
            match suggestions.iter_mut().find(|s| s.suggested == suggested) {
                // Known already: Slack can still tell them apart by their full name.
                Some(known) => known.detail = detail,
                None => suggestions.push(Suggestion {
                    suggested,
                    name: person.name.clone(),
                    detail,
                    positions: Vec::new(),
                }),
            }
        }
        suggestions.truncate(SUGGESTIONS);
        suggestions
    }

    /// Asks this Workspace, at `key`, for the names of the people `messages` name, and of the bots that wrote them
    /// without saying theirs, not known or asked for yet.
    fn want_names_in<'a>(
        &mut self,
        key: WorkspaceKey,
        messages: impl IntoIterator<Item = &'a Message>,
        commands: &mut Vec<Command>,
    ) {
        for message in messages {
            // A bot is known only once asked for.
            if let Author::UnnamedBot(bot) = &message.author
                && self.requested_bots.insert(bot.clone())
            {
                commands.push(Command::Slack(key, Request::Bot(bot.clone())));
            }
            self.want_users(key, people_in(message), commands);
        }
    }

    /// Whether everyone `message` names has a name here: its author, and the people it mentions.
    fn knows_names_in(&self, message: &Message) -> bool {
        let author = match &message.author {
            Author::UnnamedBot(bot) => self.bots.contains_key(bot),
            Author::User(_) | Author::Bot { .. } => true,
        };
        author
            && people_in(message)
                .iter()
                .all(|person| self.users.contains_key(person))
    }

    /// The people the user has DMs with, in sidebar order.
    fn dm_partners(&self) -> impl Iterator<Item = &UserId> {
        self.conversations.iter().filter_map(|c| match &c.kind {
            ConversationKind::Dm { with } => Some(with),
            ConversationKind::Channel { .. } | ConversationKind::GroupDm => None,
        })
    }

    /// Asks this Workspace, at `key`, to follow whether the people the user has DMs with are using Slack, unless
    /// it follows just them already.
    fn follow_presence(&mut self, key: WorkspaceKey, commands: &mut Vec<Command>) {
        let partners: Vec<UserId> = self.dm_partners().cloned().collect();
        if partners != self.presence_followed {
            self.presence_followed.clone_from(&partners);
            commands.push(Command::Slack(key, Request::FollowPresence(partners)));
        }
    }

    /// Whether `person` is using Slack right now; `None` when Slack hasn't said, as it only does of the people the
    /// user has DMs with.
    pub(crate) fn presence(&self, person: &UserId) -> Option<Presence> {
        self.presence.get(person).copied()
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

/// Whether `url` is an `http` or `https` link.
fn is_web(url: &str) -> bool {
    let scheme = url.split_once("://").map_or("", |(scheme, _)| scheme);
    scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
}

/// Whether `conversation` is a DM or a group DM, which the sidebar lists after the Channels.
fn is_dm(conversation: &Conversation) -> bool {
    !matches!(conversation.kind, ConversationKind::Channel { .. })
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

/// Everyone a Message names: its author and the people it mentions.
fn people_in(message: &Message) -> Vec<UserId> {
    let author = match &message.author {
        Author::User(id) => Some(id.clone()),
        Author::Bot { .. } | Author::UnnamedBot(_) => None,
    };
    let mentioned = message.content.inlines().filter_map(|inline| match inline {
        Inline::User(id) => Some(id.clone()),
        _ => None,
    });
    author.into_iter().chain(mentioned).collect()
}

/// `text` in `style`, as `mrkdwn` writes it: between `*` for bold, `_` for italic, `~` for strikethrough and `` ` ``
/// for code. Spaces at either end stay outside the markers, which only count against words.
fn styled(text: &str, style: Style) -> String {
    let markers: String = [
        (Style::BOLD, '*'),
        (Style::ITALIC, '_'),
        (Style::STRIKE, '~'),
        (Style::CODE, '`'),
    ]
    .into_iter()
    .filter(|(marker, _)| style.contains(*marker))
    .map(|(_, mark)| mark)
    .collect();
    let words = text.trim();
    if markers.is_empty() || words.is_empty() {
        return text.to_owned();
    }
    let (start, end) = (text.len() - text.trim_start().len(), text.trim_end().len());
    let closing: String = markers.chars().rev().collect();
    format!(
        "{}{markers}{words}{closing}{}",
        &text[..start],
        &text[end..]
    )
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
    use slackers_model::{ActivityItem, Emoji, MentionOf, RichText, WorkspaceId};
    use slackers_slack::{ActivityCursor, ActivityPage, HistoryPage, Part};

    use super::*;
    use crate::pictures::PictureState;
    use crate::sidebar::Row;

    const W0: WorkspaceKey = WorkspaceKey(0);
    const CELL: CellSize = CellSize {
        width: 10,
        height: 20,
    };
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
            ephemeral: false,
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
    /// What the status line tells the user now.
    fn told(app: &App) -> Option<&str> {
        app.notice(app.animation_time())
            .map(|notice| notice.text.as_str())
    }

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
        match app.sidebar.selected()?.item()? {
            Item::Conversation(conversation) => Some(conversation.clone()),
            Item::Thread { .. } => None,
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

    fn snapshot(name: &str, conversations: Vec<(Conversation, Unread)>) -> Snapshot {
        Snapshot {
            workspace: WorkspaceId::new("T1"),
            name: name.into(),
            icon: None,
            me: UserId::new("UME"),
            permalinks: Permalinks::new("https://test.slack.com/"),
            conversations,
            followed_threads: Vec::new(),
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
                (conversation("D1", dm, ""), Unread::default()),
                (
                    channel("C2", "random"),
                    Unread::counted(seconds(1), seconds(5), true, 0),
                ),
                (
                    conversation("C1", ConversationKind::Channel { private: true }, "general"),
                    Unread::default(),
                ),
            ],
        )
    }

    /// An app signed in to one Workspace, `test`, which [`test_snapshot`] shows.
    fn ready_app() -> (App, Vec<Command>) {
        let mut app = App::new(vec!["test".into()], Vec::new(), CELL);
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
        let mut app = App::new(
            vec!["test".into(), "other".into()],
            virtual_workspaces,
            CELL,
        );
        app.update(slack(Update::Ready(Box::new(test_snapshot()))));
        let other = snapshot(
            "Other",
            vec![
                (channel("C5", "general"), Unread::default()),
                (channel("C6", "dev"), Unread::default()),
            ],
        );
        app.update(Event::Slack(W1, Update::Ready(Box::new(other))));
        app
    }

    /// The sidebar, as `>` and a section's name for a header, a Conversation's name, and for a Thread its
    /// Conversation's name, `/`, and its parent's time in seconds.
    fn sidebar(app: &App) -> Vec<String> {
        let name = |conversation: &ConversationRef| {
            let (workspace, found) = app.workspaces.conversation(conversation).unwrap();
            workspace.conversation_name(found).to_owned()
        };
        app.sidebar
            .rows()
            .iter()
            .map(|row| match row {
                Row::Activity => "activity".to_owned(),
                Row::Header(Section::Threads) => ">threads".to_owned(),
                Row::Header(Section::VirtualWorkspace(name)) => format!(">{name}"),
                Row::Header(Section::Workspace(key)) => {
                    format!(">{}", app.workspaces.sign_in(*key).unwrap().label)
                }
                Row::Item {
                    item: Item::Conversation(conversation),
                    ..
                } => name(conversation),
                Row::Item {
                    item:
                        Item::Thread {
                            conversation,
                            parent,
                        },
                    ..
                } => format!("{}/{}", name(conversation), parent.as_micros() / 1_000_000),
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
        // It opened on its first unread Message: on to the newest, which reads it.
        app.update(key(KeyCode::Char('G')));
        drawn(&mut app, 10);
        app
    }

    fn unread(app: &App, conversation: &str) -> bool {
        workspace(app, W0).unread[&id(conversation)].has_unread()
    }

    #[test]
    fn signing_in_sorts_the_sidebar_and_selects_the_first_unread_without_opening_it() {
        let (app, commands) = ready_app();
        assert_eq!(
            sidebar(&app),
            ["activity", ">threads", ">test", "general", "random", "U9"]
        );
        assert_eq!(selected(&app), Some(c("C2")));
        assert_eq!(app.opened, None);
        assert_eq!(
            commands,
            [
                ask(Request::User(UserId::new("U9"))),
                ask(Request::FollowPresence(vec![UserId::new("U9")])),
                ask(Request::Activity { cursor: None })
            ]
        );
    }

    #[test]
    fn virtual_workspaces_come_first_and_take_their_channels_out_of_their_workspaces() {
        let app = two_workspaces();
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">K", "random", "dev", ">test", "general", "U9", ">other",
                "general"
            ]
        );
        assert_eq!(selected(&app), Some(c("C2")), "the unread one, in K");
    }

    #[test]
    fn workspaces_sign_in_on_their_own() {
        let mut app = App::new(vec!["test".into(), "other".into()], Vec::new(), CELL);
        assert_eq!(sidebar(&app), ["activity", ">threads", ">test", ">other"]);
        app.update(Event::CantSignIn {
            workspace: W1,
            reason: "no token".into(),
        });
        app.update(slack(Update::Ready(Box::new(test_snapshot()))));
        let other = &app.workspaces.sign_in(W1).unwrap().phase;
        assert!(matches!(other, Phase::Failed(reason) if reason == "no token"));
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">test", "general", "random", "U9", ">other"
            ]
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
            app.open_conversation().cloned(),
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
                fit: Fit::Emoji,
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
        assert_eq!(app.open_conversation().cloned(), Some(c("C1")));

        let commands = command(&mut app, "vw add k");
        let saved = &app.virtual_workspaces;
        assert_eq!(saved.len(), 1, "K, in any case");
        assert_eq!(saved[0].channels[2].to_string(), "test#general");
        assert_eq!(commands, [Command::SaveVirtualWorkspaces(saved.clone())]);
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">K", "random", "dev", "general", ">test", "U9", ">other",
                "general"
            ]
        );
        assert_eq!(app.sidebar.cursor(), 5, "the cursor follows it");

        command(&mut app, "vw add Gateway API");
        assert_eq!(app.virtual_workspaces[1].name, "Gateway API");
        assert_eq!(sidebar(&app)[6..9], [">Gateway API", "general", ">test"]);

        assert!(command(&mut app, "vw add K").is_empty());
        assert_eq!(told(&app), Some("#general is in K already"));
    }

    #[test]
    fn vw_rm_takes_it_out_and_an_empty_virtual_workspace_goes() {
        let mut app = two_workspaces();
        app.update(key(KeyCode::Enter));
        assert_eq!(app.open_conversation().cloned(), Some(c("C2")));

        let commands = command(&mut app, "vw rm K");
        assert_eq!(app.virtual_workspaces[0].channels.len(), 1);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">K", "dev", ">test", "general", "random", "U9", ">other",
                "general"
            ]
        );

        assert!(command(&mut app, "vw rm K").is_empty());
        assert_eq!(told(&app), Some("#random isn't in K"));
        keys(&mut app, "\\");
        keys(&mut app, "gjjj");
        app.update(key(KeyCode::Enter));
        command(&mut app, "vw rm K");
        assert!(app.virtual_workspaces.is_empty());
        assert_eq!(sidebar(&app)[2], ">test");
    }

    #[test]
    fn dms_stay_out_of_virtual_workspaces() {
        let mut app = two_workspaces();
        keys(&mut app, "jjjj");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.open_conversation().cloned(), Some(c("D1")));
        assert!(command(&mut app, "vw add K").is_empty());
        assert!(told(&app).unwrap().starts_with("DMs stay"));

        assert!(command(&mut app, "vw add").is_empty());
        assert_eq!(told(&app), Some("usage: :vw add NAME, or :vw rm NAME"));
    }

    #[test]
    fn z_commands_and_enter_fold_sections() {
        let mut app = two_workspaces();
        keys(&mut app, "zc");
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">K", ">test", "general", "U9", ">other", "general"
            ]
        );
        assert_eq!(app.sidebar.cursor(), 2);
        app.update(key(KeyCode::Enter));
        assert_eq!(sidebar(&app).len(), 10, "enter on a header unfolds it");

        keys(&mut app, "zM");
        assert_eq!(
            sidebar(&app),
            ["activity", ">threads", ">K", ">test", ">other"]
        );
        keys(&mut app, "jza");
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">K", ">test", "general", "U9", ">other"
            ]
        );
        keys(&mut app, "zR");
        assert_eq!(sidebar(&app).len(), 10);
        assert!(keys(&mut app, "zx").is_empty(), "not a fold command");
        assert_eq!(app.sidebar.cursor(), 5, "and no move either");
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
        assert_eq!(
            app.open_conversation().cloned(),
            Some(other_general.clone())
        );
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
        // Back to the Read Marker, at 1: nothing older is needed to open on the first unread.
        let page = HistoryPage {
            messages: vec![
                message(5, "U2", RichText::plain("hi")),
                message(2, "U2", RichText::plain("ho")),
                message(1, "U2", RichText::plain("hey")),
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
                before: Some(seconds(1))
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
        assert_eq!(told(&app), Some("open a conversation first"));
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
                text: vec![Part::Text("on it".into())],
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
            ephemeral: false,
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
            user: UserId::new("U3"),
            author: None,
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
        assert_eq!(
            (app.open_thread, app.open_conversation()),
            (None, Some(&c("C1")))
        );
    }

    #[test]
    fn a_conversation_that_appears_joins_the_sidebar_where_it_belongs() {
        let later = VirtualWorkspace {
            name: "K".into(),
            channels: vec![ChannelRef {
                workspace: "test".into(),
                channel: "later".into(),
            }],
        };
        let mut app = App::new(vec!["test".into()], vec![later], CELL);
        app.update(slack(Update::Ready(Box::new(test_snapshot()))));
        let joined = |conversation| {
            slack(Update::Live(LiveEvent::Joined {
                conversation,
                unread: Unread::default(),
            }))
        };
        let dm = ConversationKind::Dm {
            with: UserId::new("U8"),
        };
        let commands = app.update(joined(conversation("D7", dm.clone(), "")));
        assert_eq!(
            commands,
            [
                ask(Request::User(UserId::new("U8"))),
                ask(Request::FollowPresence(vec![
                    UserId::new("U8"),
                    UserId::new("U9")
                ])),
            ]
        );
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">K", ">test", "general", "random", "U8", "U9"
            ]
        );

        let commands = app.update(joined(channel("C3", "alpha")));
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, Command::Slack(_, Request::FollowPresence(_)))),
            "the same people: {commands:?}"
        );
        app.update(joined(channel("C4", "later")));
        assert_eq!(
            sidebar(&app),
            [
                "activity", ">threads", ">K", "later", ">test", "alpha", "general", "random", "U8",
                "U9"
            ],
            "into its virtual workspace, as the config says"
        );
        assert!(
            app.update(joined(conversation("D7", dm, ""))).is_empty(),
            "only once"
        );
        assert_eq!(sidebar(&app).len(), 10);

        let posted = LiveEvent::Posted {
            conversation: id("D7"),
            message: message(20, "U8", RichText::plain("hello")),
        };
        app.update(slack(Update::Live(posted)));
        assert_eq!(workspace(&app, W0).unread[&id("D7")].mentions(), 1);
    }

    /// `ready_app`, also following two Threads in `#random`: one under the Message at 10 seconds with a Reply to
    /// read, at 12; one under the Message at 20, read.
    fn following_threads() -> (App, Vec<Command>) {
        let mut app = App::new(vec!["test".into()], Vec::new(), CELL);
        let followed = |parent: u64, read: u64, latest: u64| slackers_slack::FollowedThread {
            conversation: id("C2"),
            parent: message(parent, "U2", RichText::plain("a question")),
            unread: Unread::counted(seconds(read), seconds(latest), latest > read, 0),
        };
        let snapshot = Snapshot {
            followed_threads: vec![followed(20, 21, 21), followed(10, 11, 12)],
            ..test_snapshot()
        };
        let commands = app.update(slack(Update::Ready(Box::new(snapshot))));
        (app, commands)
    }

    /// Reports a draw that showed the Thread's pane, as tall as `height`, with everything fitting.
    fn thread_drawn(app: &mut App, height: usize) -> Vec<Command> {
        let settled = Settled {
            height,
            ..Settled::default()
        };
        app.update(Event::Drawn(Drawn {
            thread: Some(settled),
            ..Drawn::default()
        }))
    }

    #[test]
    fn followed_threads_with_replies_to_read_head_the_sidebar() {
        let (app, _) = following_threads();
        assert_eq!(
            sidebar(&app),
            [
                "activity",
                ">threads",
                "random/10",
                ">test",
                "general",
                "random",
                "U9"
            ]
        );
        assert_eq!(app.sidebar.cursor(), 2, "the first thing unread");
    }

    #[test]
    fn replies_count_on_followed_threads_and_a_mention_follows_one() {
        let (mut app, _) = following_threads();
        let reply = |s: u64, parent: u64, author: &str, text: RichText| {
            let message = Message {
                reply_to: Some(seconds(parent)),
                ..message(s, author, text)
            };
            slack(Update::Live(LiveEvent::Posted {
                conversation: id("C2"),
                message,
            }))
        };
        app.update(reply(30, 20, "U2", RichText::plain("an answer")));
        assert_eq!(
            sidebar(&app)[..4],
            ["activity", ">threads", "random/20", "random/10"],
            "newest Reply first"
        );

        app.update(reply(31, 40, "U2", RichText::plain("elsewhere")));
        assert_eq!(
            sidebar(&app)[..5],
            ["activity", ">threads", "random/20", "random/10", ">test"]
        );

        let mut mention = RichText::plain("ping ");
        if let slackers_model::Block::Paragraph(inlines) = &mut mention.blocks[0] {
            inlines.push(Inline::User(UserId::new("UME")));
        }
        app.update(reply(32, 50, "U2", mention));
        assert_eq!(sidebar(&app)[..3], ["activity", ">threads", "random/50"]);
        let followed = app.workspaces.followed(&c("C2"), seconds(50)).unwrap();
        assert_eq!(followed.unread.mentions(), 1);

        app.update(reply(33, 60, "UME", RichText::plain("mine")));
        let mine = app.workspaces.followed(&c("C2"), seconds(60)).unwrap();
        assert!(!mine.unread.has_unread(), "replying follows a thread, read");
        assert!(
            unread(&app, "C2"),
            "replies leave the channel's own badge alone"
        );
    }

    #[test]
    fn a_thread_row_opens_its_thread_which_is_marked_read_once_shown() {
        let (mut app, _) = following_threads();
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            commands,
            [
                ask(Request::History {
                    conversation: id("C2"),
                    before: None
                }),
                ask(Request::Replies {
                    conversation: id("C2"),
                    parent: seconds(10)
                })
            ]
        );
        assert_eq!(
            (app.open_conversation(), app.open_thread, app.focus),
            (Some(&c("C2")), Some(seconds(10)), Focus::Thread)
        );
        let replies = vec![
            message(10, "U2", RichText::plain("a question")),
            reply(11, 10),
            reply(12, 10),
        ];
        app.update(slack(Update::Replies {
            conversation: id("C2"),
            parent: seconds(10),
            result: Ok(replies),
        }));
        assert_eq!(
            thread_drawn(&mut app, 10),
            [ask(Request::MarkThread {
                conversation: id("C2"),
                parent: seconds(10),
                up_to: seconds(12)
            })]
        );
        assert_eq!(sidebar(&app)[2], "random/10", "it stays while it's open");

        keys(&mut app, "\\");
        keys(&mut app, "G");
        app.update(key(KeyCode::Enter));
        assert_eq!(sidebar(&app)[2], ">test", "and goes once something else is");
    }

    #[test]
    fn thread_events_follow_unfollow_and_mark() {
        let mut app = reading_random();
        let live = |event| slack(Update::Live(event));
        // The parent at 5 seconds is held, and its summary says its newest Reply is at 6.
        app.update(live(LiveEvent::ThreadFollowed {
            conversation: id("C2"),
            parent: seconds(5),
            read_up_to: seconds(5),
        }));
        let followed = |app: &App| {
            app.workspaces
                .followed(&c("C2"), seconds(5))
                .map(|followed| followed.unread.has_unread())
        };
        assert_eq!(followed(&app), Some(true));

        app.update(live(LiveEvent::ThreadMarked {
            conversation: id("C2"),
            parent: seconds(5),
            up_to: seconds(6),
        }));
        assert_eq!(followed(&app), Some(false));

        app.update(live(LiveEvent::ThreadUnfollowed {
            conversation: id("C2"),
            parent: seconds(5),
        }));
        assert_eq!(followed(&app), None);
    }

    /// An Activity item `key`, at `at` seconds, about the Message at `ts` seconds in `conversation`, in the Thread of
    /// the one at `thread` seconds if any.
    fn activity_item(
        key: &str,
        at: u64,
        conversation: &str,
        ts: u64,
        thread: Option<u64>,
        happened: Happened,
    ) -> ActivityItem {
        ActivityItem {
            key: key.into(),
            at: seconds(at),
            unread: false,
            message: MessageRef {
                conversation: id(conversation),
                ts: seconds(ts),
                thread: thread.map(seconds),
            },
            happened,
        }
    }

    /// The first Workspace's newest page of Activity, holding `items` and leading to `next`.
    fn activity_page(items: Vec<ActivityItem>, next: Option<&str>) -> Event {
        let next = next.map(|cursor| ActivityCursor(cursor.into()));
        slack(Update::Activity {
            cursor: None,
            result: Ok(ActivityPage { items, next }),
        })
    }

    fn mentioned_by(author: &str) -> Happened {
        Happened::Mention {
            author: UserId::new(author),
            of: MentionOf::User,
        }
    }

    /// `ready_app` with Activity open, its first page answered empty.
    fn reading_activity() -> App {
        let (mut app, _) = ready_app();
        keys(&mut app, "g");
        app.update(key(KeyCode::Enter));
        app.update(activity_page(Vec::new(), None));
        app.update(activity_page(Vec::new(), None));
        app
    }

    #[test]
    fn activity_opens_from_the_sidebars_first_row_and_asks_for_whats_new() {
        let (mut app, commands) = ready_app();
        let newest = ask(Request::Activity { cursor: None });
        assert!(commands.contains(&newest), "signing in asks: {commands:?}");
        keys(&mut app, "g");
        assert!(
            app.update(key(KeyCode::Enter)).is_empty(),
            "the newest page is on its way already"
        );
        assert_eq!(
            (&app.opened, app.focus),
            (&Some(Opened::Activity), Focus::Activity)
        );
        let item = activity_item("m", 7, "C2", 5, None, mentioned_by("U2"));
        assert_eq!(
            app.update(activity_page(vec![item], None)),
            [ask(Request::User(UserId::new("U2"))), newest],
            "who did it, and again what's new, as opening asked while the first was on its way"
        );
        assert_eq!(app.activity.entries().len(), 1);
        assert!(keys(&mut app, "\\").is_empty());
        assert_eq!(app.focus, Focus::Sidebar);
        keys(&mut app, "\\");
        assert_eq!(app.focus, Focus::Activity, "back beside the sidebar");
    }

    #[test]
    fn an_item_opens_its_message_in_its_thread_or_its_conversation() {
        let mut app = reading_activity();
        app.update(activity_page(
            vec![
                activity_item("r", 9, "C2", 12, Some(10), Happened::Replies { unread: 2 }),
                activity_item("m", 8, "C1", 5, None, mentioned_by("U2")),
                activity_item("x", 7, "C9", 3, None, Happened::Dm),
                activity_item(
                    "y",
                    6,
                    "C2",
                    13,
                    None,
                    Happened::Reaction {
                        user: UserId::new("U3"),
                        emoji: Emoji::named("tada"),
                    },
                ),
            ],
            None,
        ));
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            commands,
            [
                ask(Request::History {
                    conversation: id("C2"),
                    before: None
                }),
                ask(Request::Replies {
                    conversation: id("C2"),
                    parent: seconds(10)
                })
            ]
        );
        assert_eq!(
            (app.open_conversation(), app.open_thread, app.focus),
            (Some(&c("C2")), Some(seconds(10)), Focus::Thread)
        );
        assert_eq!(app.thread_pane.cursor(), Some(seconds(12)));

        keys(&mut app, "\\g");
        app.update(key(KeyCode::Enter));
        keys(&mut app, "j");
        app.update(key(KeyCode::Enter));
        assert_eq!(
            (app.open_conversation(), app.open_thread, app.focus),
            (Some(&c("C1")), None, Focus::Timeline)
        );
        assert_eq!(app.conversation_pane.cursor(), Some(seconds(5)));

        // Only the Message reacted to says it's a Reply.
        app.update(slack(Update::Message {
            conversation: id("C2"),
            ts: seconds(13),
            result: Ok(Some(Message {
                reply_to: Some(seconds(10)),
                ..message(13, "UME", RichText::plain("mine"))
            })),
        }));
        keys(&mut app, "\\g");
        app.update(key(KeyCode::Enter));
        keys(&mut app, "G");
        app.update(key(KeyCode::Enter));
        assert_eq!(
            (app.open_conversation(), app.open_thread),
            (Some(&c("C2")), Some(seconds(10)))
        );
        assert_eq!(app.thread_pane.cursor(), Some(seconds(13)));

        keys(&mut app, "\\g");
        app.update(key(KeyCode::Enter));
        keys(&mut app, "k");
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert_eq!(told(&app), Some("that conversation isn't in your sidebar"));
        assert_eq!(app.opened, Some(Opened::Activity));
    }

    #[test]
    fn an_older_message_an_item_opens_is_reached_through_at_most_ten_pages_of_history() {
        let history = |commands: &[Command]| {
            commands
                .iter()
                .filter(|command| matches!(command, Command::Slack(_, Request::History { .. })))
                .count()
        };
        let page = |conversation: &str, from: u64, has_more| {
            let messages = (0..3)
                .map(|n| message(from - n, "UME", RichText::plain("then")))
                .collect();
            slack(Update::History {
                conversation: id(conversation),
                result: Ok(HistoryPage { messages, has_more }),
            })
        };
        let mut app = reading_activity();
        app.update(activity_page(
            vec![
                activity_item("near", 9, "C1", 4, None, mentioned_by("U2")),
                activity_item("far", 8, "C2", 1, None, mentioned_by("U2")),
            ],
            None,
        ));
        assert_eq!(history(&app.update(key(KeyCode::Enter))), 1);
        assert_eq!(
            history(&app.update(page("C1", 9, true))),
            1,
            "not there yet"
        );
        assert_eq!(history(&app.update(page("C1", 6, true))), 0, "there");
        assert_eq!(app.conversation_pane.cursor(), Some(seconds(4)));

        keys(&mut app, "\\g");
        app.update(key(KeyCode::Enter));
        keys(&mut app, "j");
        app.update(key(KeyCode::Enter));
        let asked: usize = (0..12)
            .map(|n| history(&app.update(page("C2", 1000 - n * 3, true))))
            .sum();
        assert_eq!(asked, 10);
    }

    #[test]
    fn whatever_concerns_the_user_asks_for_whats_new_one_request_at_a_time() {
        let mut app = reading_activity();
        let newest = ask(Request::Activity { cursor: None });
        let live = |event| slack(Update::Live(event));
        let posted_in = |conversation: &str, author: &str, text: RichText| {
            live(LiveEvent::Posted {
                conversation: id(conversation),
                message: message(20, author, text),
            })
        };
        let mut mention = RichText::plain("ping ");
        if let slackers_model::Block::Paragraph(inlines) = &mut mention.blocks[0] {
            inlines.push(Inline::User(UserId::new("UME")));
        }
        let asks = |app: &mut App, event| app.update(event).contains(&newest);
        assert!(!asks(
            &mut app,
            posted_in("C1", "U2", RichText::plain("hi"))
        ));
        assert!(
            !asks(&mut app, posted_in("D1", "UME", RichText::plain("hi"))),
            "the user's own, even in a DM"
        );
        assert!(asks(&mut app, posted_in("C1", "U2", mention.clone())));
        assert!(
            !asks(&mut app, posted_in("C1", "U3", mention)),
            "one at a time"
        );
        assert!(
            asks(&mut app, activity_page(Vec::new(), None)),
            "then again, for what came meanwhile"
        );
        app.update(activity_page(Vec::new(), None));
        assert!(
            asks(&mut app, posted_in("D1", "U9", RichText::plain("hi"))),
            "a DM"
        );
        app.update(activity_page(Vec::new(), None));

        let reacted = |author: &str, user: &str| {
            live(LiveEvent::Reacted {
                conversation: id("C1"),
                ts: seconds(20),
                emoji: Emoji::named("tada"),
                user: UserId::new(user),
                author: Some(UserId::new(author)),
                change: ReactionChange::Added,
            })
        };
        assert!(!asks(&mut app, reacted("U2", "U3")), "someone else's");
        assert!(!asks(&mut app, reacted("UME", "UME")), "the user's own");
        assert!(asks(&mut app, reacted("UME", "U3")));
        app.update(activity_page(Vec::new(), None));

        let joined = |conversation: Conversation| {
            live(LiveEvent::Joined {
                conversation,
                unread: Unread::default(),
            })
        };
        assert!(
            !asks(&mut app, joined(channel("C1", "general"))),
            "listed already"
        );
        assert!(
            asks(&mut app, joined(channel("C7", "new"))),
            "added to a Channel"
        );
        let item = ActivityItem {
            unread: true,
            ..activity_item("m", 30, "C1", 20, None, mentioned_by("U2"))
        };
        app.update(activity_page(vec![item], None));

        let marked = |conversation: &str| {
            live(LiveEvent::Marked {
                conversation: id(conversation),
                up_to: seconds(30),
            })
        };
        assert!(!asks(&mut app, marked("C2")), "nothing unread there");
        assert!(asks(&mut app, marked("C1")), "Slack may see it read now");
    }

    #[test]
    fn the_messages_items_show_are_fetched_once_unless_loaded() {
        let (mut app, _) = ready_app();
        let at = MessageRef {
            conversation: id("C1"),
            ts: seconds(5),
            thread: None,
        };
        let wanting = |at: &MessageRef| {
            Event::Drawn(Drawn {
                wanted_messages: vec![(W0, at.clone())],
                ..Drawn::default()
            })
        };
        let fetch = ask(Request::Message {
            conversation: id("C1"),
            ts: seconds(5),
        });
        assert_eq!(app.update(wanting(&at)), [fetch]);
        assert!(app.update(wanting(&at)).is_empty());
        assert_eq!(app.activity_message(W0, &at), Fetched::Fetching);
        app.update(slack(Update::Message {
            conversation: id("C1"),
            ts: seconds(5),
            result: Ok(Some(message(5, "U2", RichText::plain("hello")))),
        }));
        assert!(matches!(
            app.activity_message(W0, &at),
            Fetched::Got(found) if found.ts == seconds(5)
        ));

        let loaded = MessageRef {
            conversation: id("C2"),
            ts: seconds(2),
            thread: None,
        };
        let mut app = reading_random();
        assert!(matches!(app.activity_message(W0, &loaded), Fetched::Got(_)));
        let commands = app.update(wanting(&loaded));
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, Command::Slack(_, Request::Message { .. }))),
            "loaded already: {commands:?}"
        );
    }

    #[test]
    fn reaching_the_oldest_item_asks_for_older_ones() {
        let (mut app, _) = ready_app();
        let items = vec![
            activity_item("a", 9, "C1", 9, None, Happened::Dm),
            activity_item("b", 8, "C1", 8, None, Happened::Dm),
        ];
        app.update(activity_page(items, Some("older")));
        keys(&mut app, "g");
        app.update(key(KeyCode::Enter));
        app.update(activity_page(Vec::new(), Some("newer than the first")));
        let older = ask(Request::Activity {
            cursor: Some(ActivityCursor("older".into())),
        });
        assert!(!keys(&mut app, "k").contains(&older));
        assert_eq!(keys(&mut app, "j"), [older], "as the first page said");
        assert!(keys(&mut app, "j").is_empty(), "on its way");
    }

    #[test]
    fn live_messages_are_unread_but_not_my_own_or_replies() {
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
        let mentions =
            |app: &App, conversation: &str| workspace(app, W0).unread[&id(conversation)].mentions();
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
        let text = vec![Part::Text("hi :wave:\nthere".into())];
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

    /// The names offered to @-mention, best first.
    fn suggested(app: &App) -> Vec<String> {
        app.suggestions
            .iter()
            .flat_map(|s| &s.items)
            .map(|item| item.name.clone())
            .collect()
    }

    fn person(id: &str, name: &str, full_name: &str) -> Person {
        Person {
            id: UserId::new(id),
            name: name.into(),
            full_name: full_name.into(),
        }
    }

    #[test]
    fn an_at_offers_whom_to_mention_and_tab_or_enter_picks_one() {
        let mut app = reading_random();
        for (id, name) in [("U2", "Ada"), ("U3", "Bob"), ("UME", "Me")] {
            app.update(slack(Update::User(User {
                id: UserId::new(id),
                name: name.into(),
                full_name: String::new(),
                title: String::new(),
                time_zone: None,
                avatar: None,
            })));
        }
        keys(&mut app, "ihi ");
        assert_eq!(app.suggestions, None);
        assert!(keys(&mut app, "@").is_empty(), "nothing to search for yet");
        // Who wrote here, the Channel's broadcasts, then everyone else known; never the user.
        assert_eq!(suggested(&app), ["Ada", "here", "channel", "Bob"]);

        let commands = keys(&mut app, "a");
        assert_eq!(commands, [ask(Request::FindPeople { query: "a".into() })]);
        assert_eq!(suggested(&app), ["Ada"]);
        let found = vec![
            person("U2", "Ada", "Ada Lovelace"),
            person("U4", "Alan", "Alan Turing"),
            person("UME", "Me", "Me"),
        ];
        app.update(slack(Update::FoundPeople {
            query: "a".into(),
            result: Ok(found),
        }));
        assert_eq!(suggested(&app), ["Ada", "Alan"]);
        let details: Vec<&str> = app
            .suggestions
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|s| s.detail.as_str())
            .collect();
        assert_eq!(details, ["Ada Lovelace", "Alan Turing"]);

        app.update(key(KeyCode::Down));
        assert!(app.update(key(KeyCode::Tab)).is_empty());
        assert_eq!(app.suggestions, None);
        keys(&mut app, "and @b");
        assert_eq!(suggested(&app), ["Bob"]);
        assert!(app.update(key(KeyCode::Enter)).is_empty(), "Enter picks");
        assert_eq!(app.drafts[&place("C2", None)].text(), "hi @Alan and @Bob ");

        let commands = app.update(key(KeyCode::Enter));
        let text = vec![
            Part::Text("hi ".into()),
            Part::User(UserId::new("U4")),
            Part::Text(" and ".into()),
            Part::User(UserId::new("U3")),
            Part::Text(" ".into()),
        ];
        assert_eq!(
            commands,
            [ask(Request::Post {
                conversation: id("C2"),
                thread: None,
                text,
                id: 0
            })]
        );

        // Alan was found by name only: once posted, his name is looked up for the Message to show it.
        let mentioning = RichText {
            blocks: vec![slackers_model::Block::Paragraph(vec![Inline::User(
                UserId::new("U4"),
            )])],
        };
        let commands = app.update(slack(Update::Posted {
            id: 0,
            conversation: id("C2"),
            result: Ok(message(9, "UME", mentioning)),
        }));
        assert_eq!(commands, [ask(Request::User(UserId::new("U4")))]);
    }

    #[test]
    fn a_name_starting_with_what_is_written_beats_a_later_word_and_who_wrote_last() {
        let mut app = reading_random();
        for (id, name) in [
            ("U2", "Brad Adams"),
            ("U3", "Ada"),
            ("U4", "Benjamin Dolan"),
        ] {
            app.update(slack(Update::User(User {
                id: UserId::new(id),
                name: name.into(),
                full_name: String::new(),
                title: String::new(),
                time_zone: None,
                avatar: None,
            })));
        }
        keys(&mut app, "i@ad");
        assert_eq!(suggested(&app), ["Ada", "Brad Adams"]);
        let positions: Vec<&[usize]> = app
            .suggestions
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|s| s.positions.as_slice())
            .collect();
        assert_eq!(positions, [&[0, 1][..], &[5, 6][..]]);
    }

    #[test]
    fn a_colon_and_two_more_offer_emoji_and_tab_or_enter_picks_one() {
        let mut app = reading_random();
        let mut custom = slackers_model::CustomEmojiSet::default();
        custom.insert_image("tacocat", "https://e/tacocat.gif");
        app.update(slack(Update::Emoji(EmojiList {
            custom,
            standard: ["tada", "taco", "+1"].map(String::from).into(),
        })));
        app.update(slack(Update::EmojiUse(HashMap::from([(
            "taco".to_owned(),
            1,
        )]))));
        keys(&mut app, "inice :t");
        assert_eq!(app.suggestions, None, "two characters at least");
        assert!(keys(&mut app, "a").is_empty(), "Slack isn't asked");
        assert_eq!(suggested(&app), ["taco", "tada", "tacocat"]);

        app.update(key(KeyCode::Down));
        assert!(app.update(key(KeyCode::Tab)).is_empty());
        assert_eq!(app.suggestions, None);
        assert_eq!(app.drafts[&place("C2", None)].text(), "nice :tada: ");
        keys(&mut app, ":ta");
        assert_eq!(
            suggested(&app),
            ["tada", "taco", "tacocat"],
            "picking it counts as using it"
        );
        keys(&mut app, "co:");
        assert_eq!(app.suggestions, None, "written out in full");

        keys(&mut app, " :thumbsu");
        let offered = &app.suggestions.as_ref().unwrap().items[0];
        assert_eq!(
            (offered.name.as_str(), offered.detail.as_str()),
            ("+1", "thumbsup")
        );
        app.update(key(KeyCode::Esc));
        assert_eq!(app.suggestions, None, "only while writing");
        keys(&mut app, "i");
        assert!(app.update(key(KeyCode::Enter)).is_empty(), "Enter picks");
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            commands,
            [ask(Request::Post {
                conversation: id("C2"),
                thread: None,
                text: vec![Part::Text("nice :tada: :taco: :+1: ".into())],
                id: 0
            })]
        );
    }

    #[test]
    fn people_are_searched_for_one_name_at_a_time_and_once() {
        let mut app = reading_random();
        assert_eq!(
            keys(&mut app, "i@a"),
            [ask(Request::FindPeople { query: "a".into() })]
        );
        assert!(keys(&mut app, "l").is_empty(), "one search at a time");
        let commands = app.update(slack(Update::FoundPeople {
            query: "a".into(),
            result: Ok(Vec::new()),
        }));
        assert_eq!(commands, [ask(Request::FindPeople { query: "al".into() })]);
        let failed = app.update(slack(Update::FoundPeople {
            query: "al".into(),
            result: Err(Error::SignedOut),
        }));
        assert!(failed.is_empty(), "a failed search isn't retried");

        assert!(app.update(key(KeyCode::Backspace)).is_empty());
        assert!(keys(&mut app, "l").is_empty());
        assert_eq!(
            keys(&mut app, "E"),
            [ask(Request::FindPeople {
                query: "ale".into()
            })]
        );
    }

    #[test]
    fn only_channels_offer_their_broadcasts() {
        let (app, _) = ready_app();
        let workspace = workspace(&app, W0);
        let offered = |conversation: &str| -> Vec<String> {
            let place = place(conversation, None);
            let suggestions = workspace.suggest(&place, "");
            suggestions.into_iter().map(|s| s.name).collect()
        };
        assert_eq!(offered("C2"), ["here", "channel"]);
        assert!(offered("D1").is_empty());
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
        assert!(told(&app).unwrap().starts_with("not sent"));
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
        assert_eq!(told(&app), Some("type :q and press Enter to quit"));

        keys(&mut app, ":wq");
        assert_eq!(app.mode, Mode::Command("wq".into()));
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert_eq!(told(&app), Some("not a command: :wq"));

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
            full_name: String::new(),
            title: String::new(),
            time_zone: None,
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
        assert_eq!(app.open_conversation().cloned(), Some(c("D1")));
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
        assert_eq!((&app.mode, &app.opened), (&Mode::Normal, &None));
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
        assert_eq!(told(&app), Some("no conversation matches /zzz"));
    }

    #[test]
    fn i_needs_an_open_conversation() {
        let (mut app, _) = ready_app();
        keys(&mut app, "i");
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(told(&app), Some("open a conversation first"));
    }

    #[test]
    fn wanted_pictures_are_downloaded_once_then_prepared() {
        let (mut app, _) = ready_app();
        let mut custom = slackers_model::CustomEmojiSet::default();
        custom.insert_image("parrot", "https://e/p.gif");
        app.update(slack(Update::Emoji(EmojiList {
            custom,
            standard: Vec::new(),
        })));

        // Each picture is drawn to its own fit, which the request says.
        let fit = Fit::Within(slackers_gfx::CellBox { cols: 4, rows: 2 });
        let wanted = || {
            Event::Drawn(Drawn {
                wanted_pictures: vec![WantedPicture {
                    workspace: W0,
                    url: "https://e/p.gif".into(),
                    fit,
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
                fit
            }]
        );
    }

    #[test]
    fn pictures_off_screen_are_forgotten_once_too_many_are_kept() {
        let (mut app, _) = ready_app();
        app.pictures = Pictures::with_budget(10);
        let ids = slackers_gfx::Ids::default();
        let drawing = |urls: &[&str]| {
            let wanted = urls.iter().map(|url| WantedPicture {
                workspace: W0,
                url: (*url).into(),
                fit: Fit::Emoji,
            });
            Event::Drawn(Drawn {
                wanted_pictures: wanted.collect(),
                ..Drawn::default()
            })
        };
        app.update(drawing(&["a", "b"]));
        for url in ["a", "b"] {
            let picture = Picture::for_tests(&ids, 1, 8);
            let ready = Event::PictureReady {
                url: url.into(),
                picture,
            };
            assert!(app.update(ready).is_empty());
        }
        assert!(
            app.update(drawing(&["b"])).is_empty(),
            "a, loaded since the last draw, may not have been drawn yet"
        );
        let commands = app.update(drawing(&["b"]));
        assert!(
            matches!(commands.as_slice(), [Command::Forget(forgotten)] if forgotten.len() == 1),
            "{commands:?}"
        );
        assert_eq!(app.pictures.state("a"), PictureState::Awaiting);
        assert!(matches!(app.pictures.state("b"), PictureState::Ready(_)));
        assert_eq!(
            app.update(drawing(&["a"])),
            [ask(Request::Download { url: "a".into() })],
            "fetched again once back on screen"
        );

        let again = Event::PictureReady {
            url: "b".into(),
            picture: Picture::for_tests(&ids, 1, 8),
        };
        let commands = app.update(again);
        assert!(
            matches!(commands.as_slice(), [Command::Forget(forgotten)] if forgotten.len() == 1),
            "a second copy is forgotten: {commands:?}"
        );
    }

    /// `reading_random`, its Message at 5 seconds now showing `content`, `files` and `reactions`, with the
    /// Conversation's pane focused.
    fn reading_random_with(
        content: RichText,
        files: Vec<slackers_model::File>,
        reactions: Vec<slackers_model::Reaction>,
    ) -> App {
        let mut app = reading_random();
        let message = Message {
            files,
            reactions,
            ..message(5, "U2", content)
        };
        app.update(slack(Update::Live(LiveEvent::Edited {
            conversation: id("C2"),
            message,
        })));
        app.focus = Focus::Timeline;
        app
    }

    /// `reading_random` with the user's own Message at 7 seconds saying `content`, `Ana` named, and the pane focused
    /// on it.
    fn reading_my_message(content: RichText) -> App {
        let mut app = reading_random();
        app.update(named("U2", "Ana"));
        app.update(slack(Update::Live(LiveEvent::Posted {
            conversation: id("C2"),
            message: message(7, "UME", content),
        })));
        app.focus = Focus::Timeline;
        app
    }

    fn text(text: &str, style: Style) -> Inline {
        Inline::Text {
            text: text.into(),
            style,
        }
    }

    #[test]
    fn e_edits_ones_own_message_as_one_would_write_it_and_enter_saves_it() {
        let mut thumbs = Emoji::named("+1");
        thumbs.skin_tone = Some(2);
        let content = RichText {
            blocks: vec![
                Block::Paragraph(vec![
                    text("fix ", Style::PLAIN),
                    text("this ", Style::BOLD),
                    Inline::Emoji(thumbs),
                    text(" ", Style::PLAIN),
                    Inline::User(UserId::new("U2")),
                    text(" in ", Style::PLAIN),
                    Inline::Channel(id("C1")),
                    text(" see ", Style::PLAIN),
                    Inline::Link {
                        url: "https://x.org/docs".into(),
                        label: Some("the docs".into()),
                    },
                    text(" or ", Style::PLAIN),
                    Inline::Link {
                        url: "https://x.org".into(),
                        label: None,
                    },
                ]),
                Block::Quote(vec![text("quoted\nmore", Style::PLAIN)]),
                Block::Preformatted("let x = 1;".into()),
                Block::List {
                    ordered: false,
                    items: vec![vec![text("a", Style::PLAIN)], vec![text("b", Style::PLAIN)]],
                },
            ],
        };
        let mut app = reading_my_message(content);
        keys(&mut app, "ihalf written");
        app.update(key(KeyCode::Esc));
        assert!(keys(&mut app, "e").is_empty());
        assert_eq!(app.mode, Mode::Insert);
        let place = place("C2", None);
        assert!(app.is_editing(&place));
        assert_eq!(
            app.drafts[&place].text(),
            "fix *this* :+1::skin-tone-2: @Ana in #general see the docs or https://x.org\n\
             > quoted\n> more\n```\nlet x = 1;\n```\n• a\n• b"
        );

        keys(&mut app, "!");
        let commands = app.update(key(KeyCode::Enter));
        let edit = Request::Edit {
            conversation: id("C2"),
            ts: seconds(7),
            text: vec![
                Part::Text("fix *this* :+1::skin-tone-2: ".into()),
                Part::User(UserId::new("U2")),
                Part::Text(" in ".into()),
                Part::Channel(id("C1")),
                Part::Text(" see ".into()),
                Part::Link {
                    url: "https://x.org/docs".into(),
                    label: "the docs".into(),
                },
                Part::Text(
                    " or https://x.org\n> quoted\n> more\n```\nlet x = 1;\n```\n• a\n• b!".into(),
                ),
            ],
        };
        assert_eq!(commands, [ask(edit)]);
        assert!(!app.is_editing(&place));
        assert_eq!(
            app.drafts[&place].text(),
            "half written",
            "what was set aside is back"
        );

        app.update(slack(Update::Edited {
            conversation: id("C2"),
            ts: seconds(7),
            result: Ok(RichText::plain("fixed")),
        }));
        let (_, edited) = app.message(&place, seconds(7)).unwrap();
        assert_eq!(
            (&edited.content, edited.edited),
            (&RichText::plain("fixed"), true)
        );
    }

    #[test]
    fn styles_write_as_markers_around_the_words_only() {
        assert_eq!(styled("plain ", Style::PLAIN), "plain ");
        assert_eq!(styled(" bold ", Style::BOLD), " *bold* ");
        assert_eq!(
            styled(
                "all of it",
                Style::BOLD | Style::ITALIC | Style::STRIKE | Style::CODE
            ),
            "*_~`all of it`~_*"
        );
        assert_eq!(styled("  ", Style::BOLD), "  ", "no words, no markers");
    }

    #[test]
    fn up_in_an_empty_composer_edits_the_newest_of_ones_own_and_esc_gives_it_up() {
        let mut app = reading_my_message(RichText::plain("first"));
        app.update(slack(Update::Live(LiveEvent::Posted {
            conversation: id("C2"),
            message: message(8, "U2", RichText::plain("someone else's, newer")),
        })));
        let place = place("C2", None);
        keys(&mut app, "ix");
        app.update(key(KeyCode::Up));
        assert!(!app.is_editing(&place), "not with something written");
        app.update(key(KeyCode::Backspace));
        app.update(key(KeyCode::Up));
        assert!(app.is_editing(&place));
        assert_eq!(app.drafts[&place].text(), "first", "the user's own");
        app.update(key(KeyCode::Esc));
        assert!(!app.is_editing(&place));
        assert_eq!(app.mode, Mode::Normal);
        assert!(
            app.drafts.get(&place).is_none_or(Draft::is_empty),
            "nothing was set aside"
        );

        keys(&mut app, "kk");
        keys(&mut app, "e");
        assert!(!app.is_editing(&place));
        assert_eq!(told(&app), Some("only your own messages can be edited"));
    }

    #[test]
    fn a_blank_edit_isnt_saved_and_a_refused_one_comes_back() {
        let mut app = reading_my_message(RichText::plain("typo"));
        let place = place("C2", None);
        keys(&mut app, "e");
        for _ in 0.."typo".len() {
            app.update(key(KeyCode::Backspace));
        }
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert!(app.is_editing(&place), "still editing");
        keys(&mut app, "fixed");
        app.update(key(KeyCode::Enter));
        app.update(key(KeyCode::Esc));

        app.update(slack(Update::Edited {
            conversation: id("C2"),
            ts: seconds(7),
            result: Err(Error::Refused {
                method: "chat.update",
                code: "edit_window_closed".into(),
            }),
        }));
        assert!(app.is_editing(&place));
        assert_eq!(app.drafts[&place].text(), "fixed");
        assert_eq!(
            told(&app),
            Some("not edited, back in the composer: slack refused chat.update: edit_window_closed")
        );
        let (_, kept) = app.message(&place, seconds(7)).unwrap();
        assert_eq!(kept.content, RichText::plain("typo"));
    }

    /// `reading_random` with `/giphy` and `/remind` on offer.
    fn with_commands() -> App {
        let mut app = reading_random();
        let command = |name: &str, usage: &str, description: &str| SlashCommand {
            name: name.into(),
            usage: usage.into(),
            description: description.into(),
        };
        app.update(slack(Update::Commands(vec![
            command("/app-remind", "", "Another app's reminders"),
            command("/giphy", "[text]", "Post a random gif to channel"),
            command("/remind", "", "Set a reminder"),
        ])));
        app
    }

    fn run(command: &str, text: Vec<Part>, thread: Option<u64>, request: u64) -> Command {
        ask(Request::RunCommand {
            conversation: id("C2"),
            thread: thread.map(seconds),
            command: command.into(),
            text,
            id: request,
        })
    }

    /// A Message posted live in `conversation` at `ts`, by `author`.
    fn posted_at(conversation: &str, ts: Ts, author: &str) -> Event {
        slack(Update::Live(LiveEvent::Posted {
            conversation: id(conversation),
            message: Message {
                ts,
                ..message(1, author, RichText::plain("hi"))
            },
        }))
    }

    /// `seconds` from now, by this machine's clock; negative for the past.
    fn from_now(seconds: i64) -> Ts {
        Ts::from_micros(now().as_micros().saturating_add_signed(seconds * 1_000_000))
    }

    #[test]
    fn a_command_that_posts_for_the_user_elsewhere_is_followed_there() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        keys(&mut app, "i/dm hi");
        app.update(key(KeyCode::Enter));
        let soon = from_now(1);

        for (conversation, ts, author, what) in [
            ("C2", soon, "UME", "where it ran"),
            ("D1", soon, "U9", "someone else's"),
            ("D1", from_now(-60), "UME", "before it ran"),
            ("D1", from_now(60), "UME", "long after"),
        ] {
            app.update(posted_at(conversation, ts, author));
            assert_eq!(app.open_conversation(), Some(&c("C2")), "{what}");
        }

        // Slack's clock, which times it, may be a little behind.
        let just_before = from_now(-2);
        app.update(posted_at("D1", just_before, "UME"));
        assert_eq!(app.open_conversation(), Some(&c("D1")));
        assert_eq!(
            app.conversation_pane.cursor(),
            Some(just_before),
            "on the post"
        );
        assert_eq!(app.mode, Mode::Insert, "writing on, there");
        assert_eq!(app.writing_place(), Some(place("D1", None)));

        app.update(posted_at("C1", soon, "UME"));
        assert_eq!(app.open_conversation(), Some(&c("D1")), "only once");
    }

    #[test]
    fn a_command_is_followed_into_a_thread_but_not_where_the_user_went_already() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        keys(&mut app, "i/msg hi");
        app.update(key(KeyCode::Enter));
        app.update(slack(Update::Live(LiveEvent::Posted {
            conversation: id("C1"),
            message: Message {
                ts: from_now(1),
                reply_to: Some(seconds(3)),
                ..message(1, "UME", RichText::plain("hi"))
            },
        })));
        assert_eq!(app.open_conversation(), Some(&c("C1")));
        assert_eq!(app.open_thread, Some(seconds(3)), "a Reply, in its Thread");

        let mut app = reading_random();
        app.focus = Focus::Timeline;
        keys(&mut app, "i/dm hi");
        app.update(key(KeyCode::Enter));
        app.open(c("D1"), &mut Vec::new());
        app.update(posted_at("D1", from_now(1), "UME"));
        assert_eq!(app.open_conversation(), Some(&c("D1")));
        assert_eq!(
            app.conversation_pane.cursor(),
            None,
            "the user got there first: left as it is"
        );
    }

    #[test]
    fn a_command_is_followed_within_its_workspace_only() {
        let mut app = two_workspaces();
        app.update(key(KeyCode::Enter));
        assert_eq!(app.open_conversation(), Some(&c("C2")));
        app.focus = Focus::Timeline;
        keys(&mut app, "i/dm hi");
        app.update(key(KeyCode::Enter));
        app.update(Event::Slack(
            W1,
            Update::Live(LiveEvent::Posted {
                conversation: id("C5"),
                message: Message {
                    ts: from_now(1),
                    ..message(1, "UME", RichText::plain("hi"))
                },
            }),
        ));
        assert_eq!(app.open_conversation(), Some(&c("C2")));
    }

    #[test]
    fn typing_shows_where_it_is_until_the_typist_posts_there() {
        let mut app = reading_random();
        let typing = |user: &str, thread: Option<u64>| {
            slack(Update::Live(LiveEvent::Typing {
                conversation: id("C2"),
                thread: thread.map(seconds),
                user: UserId::new(user),
            }))
        };
        let commands = app.update(typing("U3", None));
        assert!(
            commands.contains(&ask(Request::User(UserId::new("U3")))),
            "to name them"
        );
        app.update(typing("UME", None));
        app.update(typing("U2", Some(5)));
        let who = |app: &App, thread| -> Vec<UserId> {
            let now = app.animation_time();
            let typists = app.typing.at(&place("C2", thread), now);
            typists.who.into_iter().cloned().collect()
        };
        assert_eq!(who(&app, None), [UserId::new("U3")], "not the user's own");
        assert_eq!(who(&app, Some(5)), [UserId::new("U2")]);

        app.update(slack(Update::Live(LiveEvent::Posted {
            conversation: id("C2"),
            message: Message {
                reply_to: Some(seconds(5)),
                ..message(8, "U2", RichText::plain("done"))
            },
        })));
        assert!(who(&app, Some(5)).is_empty());
        assert_eq!(who(&app, None), [UserId::new("U3")]);
        app.update(posted_at("C2", seconds(9), "U3"));
        assert!(who(&app, None).is_empty());
    }

    #[test]
    fn presence_is_kept_as_slack_says() {
        let (mut app, _) = ready_app();
        let presence = |people: &[&str], presence| {
            slack(Update::Live(LiveEvent::Presence {
                people: people.iter().map(|&id| UserId::new(id)).collect(),
                presence,
            }))
        };
        let of = |app: &App, id: &str| workspace(app, W0).presence(&UserId::new(id));
        assert_eq!(of(&app, "U9"), None);
        app.update(presence(&["U9", "U2"], Presence::Active));
        assert_eq!(
            (of(&app, "U9"), of(&app, "U2")),
            (Some(Presence::Active), Some(Presence::Active))
        );
        app.update(presence(&["U9"], Presence::Away));
        assert_eq!(
            (of(&app, "U9"), of(&app, "U2")),
            (Some(Presence::Away), Some(Presence::Active))
        );
    }

    #[test]
    fn shift_u_marks_messages_unread_and_the_conversation_stays_so_while_open() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        let unread = |app: &App| workspace(app, W0).unread[&id("C2")].clone();
        assert!(!unread(&app).has_unread(), "read once shown");
        app.update(key(KeyCode::Char('k')));
        assert_eq!(app.conversation_pane.cursor(), Some(seconds(2)));
        let marker = Ts::from_micros(seconds(2).as_micros() - 1);
        assert_eq!(
            app.update(key(KeyCode::Char('U'))),
            [ask(Request::Mark {
                conversation: id("C2"),
                up_to: marker
            })]
        );
        assert!(unread(&app).has_unread());

        app.update(key(KeyCode::Char('j')));
        assert!(
            drawn(&mut app, 10).is_empty(),
            "the newest on screen doesn't read it"
        );
        app.update(slack(Update::Live(LiveEvent::Marked {
            conversation: id("C2"),
            up_to: marker,
        })));
        assert_eq!(unread(&app).read_up_to(), marker, "Slack's echo");

        app.open(c("C1"), &mut Vec::new());
        app.open(c("C2"), &mut Vec::new());
        assert!(drawn(&mut app, 10).is_empty());
        assert_eq!(
            app.conversation_pane.cursor(),
            Some(seconds(2)),
            "opened again, on the first unread"
        );
        assert_eq!(
            app.update(key(KeyCode::Char('G'))),
            [ask(Request::Mark {
                conversation: id("C2"),
                up_to: seconds(5)
            })],
            "read once the newest is reached"
        );
    }

    #[test]
    fn a_threads_replies_cant_be_marked_unread() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        app.update(key(KeyCode::Enter));
        app.update(slack(Update::Replies {
            conversation: id("C2"),
            parent: seconds(5),
            result: Ok(vec![message(5, "U2", RichText::plain("hi")), reply(6, 5)]),
        }));
        assert_eq!(app.focus, Focus::Thread);
        assert!(app.update(key(KeyCode::Char('U'))).is_empty());
        assert_eq!(
            told(&app),
            Some("only a conversation's messages can be marked unread")
        );
    }

    #[test]
    fn a_notice_shows_for_a_while_unless_another_takes_its_place() {
        let (mut app, _) = ready_app();
        command(&mut app, "bogus");
        let now = app.animation_time();
        let notice = app.notice(now).expect("told").clone();
        assert_eq!(notice.text, "not a command: :bogus");
        assert!(notice.until > now + Duration::from_secs(7), "{notice:?}");
        assert!(
            app.notice(notice.until - Duration::from_millis(1))
                .is_some()
        );
        assert_eq!(
            app.notice(notice.until),
            None,
            "gone once it's had its time"
        );

        command(&mut app, "other");
        let newer = app.notice(app.animation_time()).expect("told").clone();
        assert_eq!(newer.text, "not a command: :other");
        assert!(newer.until >= notice.until, "for a while of its own");
    }

    #[test]
    fn a_virtual_workspace_listing_a_channel_the_user_isnt_in_says_so() {
        let listed = |channel: &str| ChannelRef {
            workspace: "test".into(),
            channel: channel.into(),
        };
        let k = VirtualWorkspace {
            name: "K".into(),
            channels: vec![listed("random"), listed("gone"), listed("nowhere")],
        };
        let mut app = App::new(vec!["test".into()], vec![k], CELL);
        app.update(slack(Update::Ready(Box::new(test_snapshot()))));
        assert_eq!(
            told(&app),
            Some("K lists test#nowhere, which isn't among your channels"),
            "the last of them: there's room for one"
        );
    }

    #[test]
    fn a_conversation_opens_on_its_first_unread_loading_back_to_it_and_reads_at_the_newest() {
        let (mut app, _) = ready_app();
        app.update(key(KeyCode::Enter));
        let page = |seconds: &[u64]| {
            slack(Update::History {
                conversation: id("C2"),
                result: Ok(HistoryPage {
                    messages: seconds
                        .iter()
                        .map(|&s| message(s, "U2", RichText::plain("hi")))
                        .collect(),
                    has_more: true,
                }),
            })
        };
        let older = |before| {
            ask(Request::History {
                conversation: id("C2"),
                before: Some(seconds(before)),
            })
        };
        let histories = |commands: Vec<Command>| -> Vec<Command> {
            commands
                .into_iter()
                .filter(|command| matches!(command, Command::Slack(_, Request::History { .. })))
                .collect()
        };
        // The Read Marker is at 1.
        assert_eq!(histories(app.update(page(&[5, 4]))), [older(4)]);
        for _ in 0..=JUMP_PAGES {
            drawn(&mut app, 10);
        }
        assert_eq!(
            histories(app.update(page(&[3, 2]))),
            [older(2)],
            "waiting for a page uses none up"
        );
        assert!(histories(app.update(page(&[1]))).is_empty(), "reached");
        assert_eq!(app.conversation_pane.cursor(), Some(seconds(2)));
        assert_eq!(app.conversation_pane.new_after(), Some(seconds(1)));
        assert!(
            !drawn(&mut app, 10)
                .iter()
                .any(|command| matches!(command, Command::Slack(_, Request::Mark { .. }))),
            "not read yet"
        );
        assert_eq!(
            app.update(key(KeyCode::Char('G'))),
            [ask(Request::Mark {
                conversation: id("C2"),
                up_to: seconds(5)
            })]
        );
        assert_eq!(
            app.conversation_pane.new_after(),
            Some(seconds(1)),
            "what was new stays marked while it's open"
        );
    }

    #[test]
    fn reaching_back_for_the_first_unread_gives_up_on_the_oldest_loaded() {
        let (mut app, _) = ready_app();
        app.update(key(KeyCode::Enter));
        // Never back to the Read Marker at 1: a Message every 10 ms, 4 seconds on. The first page, then each asked
        // for.
        let mut requests = 0;
        for page in 0..=JUMP_PAGES {
            let at = Ts::from_micros(4_000_000 - u64::from(page) * 10_000);
            let commands = app.update(slack(Update::History {
                conversation: id("C2"),
                result: Ok(HistoryPage {
                    messages: vec![Message {
                        ts: at,
                        ..message(1, "U2", RichText::plain("hi"))
                    }],
                    has_more: true,
                }),
            }));
            requests += commands
                .iter()
                .filter(|command| matches!(command, Command::Slack(_, Request::History { .. })))
                .count();
        }
        assert_eq!(requests, usize::try_from(JUMP_PAGES).unwrap());
        let oldest = workspace(&app, W0).timelines[&id("C2")].oldest();
        assert_eq!(
            app.conversation_pane.cursor(),
            oldest,
            "as far back as it got"
        );
    }

    #[test]
    fn a_followed_thread_opens_on_its_first_unread_reply() {
        let mut snapshot = test_snapshot();
        snapshot
            .followed_threads
            .push(slackers_slack::FollowedThread {
                conversation: id("C1"),
                parent: message(5, "U2", RichText::plain("hi")),
                unread: Unread::counted(seconds(6), seconds(8), true, 0),
            });
        let mut app = App::new(vec!["test".into()], Vec::new(), CELL);
        app.update(slack(Update::Ready(Box::new(snapshot))));
        app.open(c("C1"), &mut Vec::new());
        app.open_thread(seconds(5), &mut Vec::new());
        assert_eq!(app.thread_pane.new_after(), Some(seconds(6)));
        let replies = vec![
            message(5, "U2", RichText::plain("hi")),
            reply(6, 5),
            reply(7, 5),
            reply(8, 5),
        ];
        app.update(slack(Update::Replies {
            conversation: id("C1"),
            parent: seconds(5),
            result: Ok(replies),
        }));
        assert_eq!(app.thread_pane.cursor(), Some(seconds(7)));

        app.open_thread(seconds(9), &mut Vec::new());
        assert_eq!(app.thread_pane.new_after(), None, "a Thread not followed");
    }

    #[test]
    fn a_command_that_failed_isnt_followed() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        keys(&mut app, "i/dm hi");
        let sent = app.update(key(KeyCode::Enter));
        let [Command::Slack(_, Request::RunCommand { id: request, .. })] = sent.as_slice() else {
            panic!("{sent:?}");
        };
        app.update(slack(Update::RanCommand {
            id: *request,
            result: Err(Error::Refused {
                method: "chat.command",
                code: "invalid_command".into(),
            }),
        }));
        app.update(posted_at("D1", from_now(1), "UME"));
        assert_eq!(app.open_conversation(), Some(&c("C2")));
    }

    #[test]
    fn a_slash_offers_the_workspaces_commands_and_enter_runs_one() {
        let mut app = with_commands();
        keys(&mut app, "i/re");
        assert_eq!(
            suggested(&app),
            ["/remind", "/app-remind"],
            "names it starts first"
        );
        app.update(key(KeyCode::Backspace));
        app.update(key(KeyCode::Backspace));
        assert_eq!(suggested(&app), ["/app-remind", "/giphy", "/remind"]);
        keys(&mut app, "gi");
        let offered = &app.suggestions.as_ref().unwrap().items;
        assert_eq!(
            (
                offered[0].name.as_str(),
                offered[0].detail.as_str(),
                &offered[0].positions[..]
            ),
            (
                "/giphy",
                "[text]  Post a random gif to channel",
                &[1, 2][..]
            )
        );
        app.update(key(KeyCode::Tab));
        keys(&mut app, "cats");
        assert_eq!(app.drafts[&place("C2", None)].text(), "/giphy cats");
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            commands,
            [run("/giphy", vec![Part::Text("cats".into())], None, 0)]
        );
        assert!(app.drafts[&place("C2", None)].is_empty());
        assert!(app.outbox.is_empty(), "nothing posted");

        let reply = RichText::plain("Here's your gif");
        app.update(slack(Update::RanCommand {
            id: 0,
            result: Ok(Some(reply)),
        }));
        assert_eq!(told(&app), Some("Here's your gif"));
    }

    #[test]
    fn an_unknown_command_stays_in_the_draft_and_a_space_posts_it_as_text() {
        let mut app = with_commands();
        keys(&mut app, "i/gif cats");
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        assert_eq!(
            told(&app),
            Some("no /gif command here: start with a space to post it as text")
        );
        assert_eq!(app.drafts[&place("C2", None)].text(), "/gif cats");

        for _ in 0.."/gif cats".len() {
            app.update(key(KeyCode::Backspace));
        }
        keys(&mut app, " /gif cats");
        let commands = app.update(key(KeyCode::Enter));
        assert!(
            matches!(
                commands.as_slice(),
                [Command::Slack(_, Request::Post { .. })]
            ),
            "{commands:?}"
        );
    }

    #[test]
    fn a_command_runs_in_its_thread_and_a_failed_one_comes_back() {
        let mut app = with_commands();
        app.update(key(KeyCode::Enter));
        keys(&mut app, "i/remind me");
        let commands = app.update(key(KeyCode::Enter));
        assert_eq!(
            commands,
            [run("/remind", vec![Part::Text("me".into())], Some(5), 0)]
        );
        app.update(slack(Update::RanCommand {
            id: 0,
            result: Err(Error::Refused {
                method: "chat.command",
                code: "restricted_action".into(),
            }),
        }));
        assert_eq!(
            told(&app),
            Some(
                "couldn't run /remind, back in the draft: slack refused chat.command: restricted_action"
            )
        );
        assert_eq!(app.drafts[&place("C2", Some(5))].text(), "/remind me");

        let (mut app, _) = ready_app();
        app.update(key(KeyCode::Enter));
        keys(&mut app, "i/whatever");
        assert!(
            matches!(
                app.update(key(KeyCode::Enter)).as_slice(),
                [Command::Slack(_, Request::RunCommand { .. })]
            ),
            "until Slack lists its commands, it says whether there's one"
        );
    }

    fn named(id: &str, name: &str) -> Event {
        slack(Update::User(User {
            id: UserId::new(id),
            name: name.into(),
            full_name: String::new(),
            title: String::new(),
            time_zone: None,
            avatar: None,
        }))
    }

    fn notified(commands: &[Command]) -> Vec<(&str, &str)> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::Notify { title, body } => Some((title.as_str(), body.as_str())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dms_mentions_and_followed_replies_notify_while_the_window_is_elsewhere() {
        let (mut app, _) = following_threads();
        for (id, name) in [("U2", "Bob"), ("U9", "Ana"), ("UME", "Me")] {
            app.update(named(id, name));
        }
        let posted = |conversation: &str, message: Message| {
            slack(Update::Live(LiveEvent::Posted {
                conversation: id(conversation),
                message,
            }))
        };
        let dm = |s: u64| posted("D1", message(s, "U9", RichText::plain("lunch?")));
        assert!(
            notified(&app.update(dm(30))).is_empty(),
            "the user is looking"
        );

        app.update(Event::Terminal(TerminalEvent::FocusLost));
        assert_eq!(notified(&app.update(dm(31))), [("Ana", "lunch?")]);
        let mine = posted("D1", message(32, "UME", RichText::plain("sure")));
        assert!(notified(&app.update(mine)).is_empty(), "the user's own");
        let chatter = posted("C1", message(33, "U2", RichText::plain("hi all")));
        assert!(
            notified(&app.update(chatter)).is_empty(),
            "nothing for the user"
        );

        let mut mention = RichText::plain("ping ");
        if let slackers_model::Block::Paragraph(inlines) = &mut mention.blocks[0] {
            inlines.push(Inline::User(UserId::new("UME")));
        }
        let mentioning = posted("C1", message(34, "U2", mention));
        assert_eq!(
            notified(&app.update(mentioning)),
            [("Bob in #general", "ping @Me")]
        );
        let reply = Message {
            reply_to: Some(seconds(10)),
            ..message(35, "U2", RichText::plain("an answer"))
        };
        assert_eq!(
            notified(&app.update(posted("C2", reply))),
            [("Bob in a thread in #random", "an answer")]
        );
    }

    #[test]
    fn a_notification_waits_for_the_names_in_it_unless_the_user_is_back() {
        let (mut app, _) = ready_app();
        app.update(Event::Terminal(TerminalEvent::FocusLost));
        let dm = |s: u64| {
            slack(Update::Live(LiveEvent::Posted {
                conversation: id("D1"),
                message: message(s, "U9", RichText::plain("lunch?")),
            }))
        };
        assert!(
            notified(&app.update(dm(30))).is_empty(),
            "U9 has no name yet"
        );
        assert_eq!(
            notified(&app.update(named("U9", "Ana"))),
            [("Ana", "lunch?")]
        );
        assert!(notified(&app.update(named("U9", "Ana"))).is_empty(), "once");

        let (mut app, _) = ready_app();
        app.update(Event::Terminal(TerminalEvent::FocusLost));
        app.update(dm(30));
        app.update(Event::Terminal(TerminalEvent::FocusGained));
        assert!(
            notified(&app.update(named("U9", "Ana"))).is_empty(),
            "moot once the user is back"
        );

        let (mut app, _) = ready_app();
        app.update(Event::Terminal(TerminalEvent::FocusLost));
        for s in 1..=25 {
            app.update(slack(Update::Live(LiveEvent::Posted {
                conversation: id("D1"),
                message: message(s, "U9", RichText::plain(s.to_string())),
            })));
        }
        let commands = app.update(named("U9", "Ana"));
        let bodies: Vec<&str> = notified(&commands)
            .into_iter()
            .map(|(_, body)| body)
            .collect();
        assert_eq!(bodies.len(), HELD_NOTIFICATIONS);
        assert_eq!((bodies[0], bodies[19]), ("6", "25"), "the oldest went");
    }

    #[test]
    fn bots_that_dont_say_their_names_are_looked_up_once_and_named_after() {
        let giphy = BotId::new("B1");
        let by_giphy = |s: u64| Message {
            author: Author::UnnamedBot(giphy.clone()),
            ..message(s, "U2", RichText::plain("a gif"))
        };
        let look_up = ask(Request::Bot(giphy.clone()));
        let mut app = reading_random();
        let page = HistoryPage {
            messages: vec![by_giphy(1), by_giphy(2)],
            has_more: false,
        };
        let commands = app.update(slack(Update::History {
            conversation: id("C2"),
            result: Ok(page),
        }));
        assert_eq!(
            commands
                .iter()
                .filter(|&command| *command == look_up)
                .count(),
            1
        );
        let posted = |s| {
            slack(Update::Live(LiveEvent::Posted {
                conversation: id("C2"),
                message: by_giphy(s),
            }))
        };
        assert!(!app.update(posted(3)).contains(&look_up), "asked already");
        let author = Author::UnnamedBot(giphy.clone());
        assert_eq!(workspace(&app, W0).author_name(&author), "bot");

        app.update(slack(Update::Bot(Bot {
            id: giphy.clone(),
            name: "giphy".into(),
            avatar: Some("https://a/giphy.png".into()),
        })));
        assert_eq!(workspace(&app, W0).author_name(&author), "giphy");
        assert_eq!(
            workspace(&app, W0).avatar(&author),
            Some("https://a/giphy.png")
        );
        assert!(!app.update(posted(4)).contains(&look_up), "known now");

        let other = BotId::new("B2");
        let fetched = app.update(slack(Update::Message {
            conversation: id("C2"),
            ts: seconds(5),
            result: Ok(Some(Message {
                author: Author::UnnamedBot(other.clone()),
                ..message(5, "U2", RichText::plain("for Activity"))
            })),
        }));
        assert_eq!(
            fetched,
            [ask(Request::Bot(other))],
            "a Message fetched for Activity too"
        );
    }

    #[test]
    fn a_bots_notification_waits_for_its_name() {
        let (mut app, _) = ready_app();
        app.update(Event::Terminal(TerminalEvent::FocusLost));
        let giphy = BotId::new("B1");
        let commands = app.update(slack(Update::Live(LiveEvent::Posted {
            conversation: id("D1"),
            message: Message {
                author: Author::UnnamedBot(giphy.clone()),
                ..message(30, "U9", RichText::plain("a gif"))
            },
        })));
        assert!(notified(&commands).is_empty(), "the bot has no name yet");
        assert!(commands.contains(&ask(Request::Bot(giphy.clone()))));
        let named = app.update(slack(Update::Bot(Bot {
            id: giphy,
            name: "giphy".into(),
            avatar: None,
        })));
        assert_eq!(notified(&named), [("giphy", "a gif")]);
    }

    #[test]
    fn with_several_workspaces_a_notification_names_its_own() {
        let mut app = two_workspaces();
        app.update(Event::Terminal(TerminalEvent::FocusLost));
        app.update(named("U9", "Ana"));
        let dm = slack(Update::Live(LiveEvent::Posted {
            conversation: id("D1"),
            message: message(30, "U9", RichText::plain("hi")),
        }));
        assert_eq!(notified(&app.update(dm)), [("Ana · Test", "hi")]);
    }

    /// `reading_random_with` its Message holding `reactions`, and the emoji `tada`, `taco` and `+1` on offer, `taco`
    /// used three times and `+1` once.
    fn reacting_to(reactions: Vec<slackers_model::Reaction>) -> App {
        let mut app = reading_random_with(RichText::plain("lunch?"), Vec::new(), reactions);
        app.update(slack(Update::Emoji(EmojiList {
            custom: slackers_model::CustomEmojiSet::default(),
            standard: ["tada", "taco", "+1"].map(String::from).into(),
        })));
        let used = HashMap::from([("taco".to_owned(), 3), ("+1".to_owned(), 1)]);
        app.update(slack(Update::EmojiUse(used)));
        app
    }

    /// The emoji to react with, best first, and what each says beside it.
    fn choices(app: &App) -> Vec<(String, String)> {
        let Mode::React(reacting) = &app.mode else {
            return Vec::new();
        };
        reacting
            .choices
            .iter()
            .flat_map(|choices| &choices.items)
            .map(|item| (item.name.clone(), item.detail.clone()))
            .collect()
    }

    /// The first Workspace's `#random` Message at 5 seconds' reactions, as emoji name and how many reacted, and
    /// whether the user is among them.
    fn reactions(app: &App) -> Vec<(String, u32, bool)> {
        let place = place("C2", None);
        let (_, message) = app.message(&place, seconds(5)).unwrap();
        message
            .reactions
            .iter()
            .map(|reaction| {
                let mine = reaction.users.contains(&UserId::new("UME"));
                (reaction.emoji.name.clone(), reaction.count, mine)
            })
            .collect()
    }

    fn react_request(name: &str, change: ReactionChange) -> Command {
        ask(Request::React {
            conversation: id("C2"),
            ts: seconds(5),
            emoji: Emoji::named(name),
            change,
        })
    }

    #[test]
    fn plus_offers_the_emoji_used_most_and_typing_narrows_them_down() {
        let mut app = reacting_to(Vec::new());
        assert!(keys(&mut app, "+").is_empty());
        let names =
            |app: &App| -> Vec<String> { choices(app).into_iter().map(|(name, _)| name).collect() };
        assert_eq!(names(&app), ["taco", "+1", "tada"]);
        keys(&mut app, ":ta");
        assert_eq!(
            names(&app),
            ["taco", "tada"],
            "a colon is no part of a name"
        );
        app.update(key(KeyCode::Down));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [react_request("tada", ReactionChange::Added)]
        );
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            reactions(&app),
            [("tada".to_owned(), 1, true)],
            "shown straight away"
        );

        keys(&mut app, "+tq");
        assert_eq!(choices(&app), [], "nothing by that name");
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        app.update(key(KeyCode::Backspace));
        assert_eq!(
            choices(&app)[..2],
            [
                ("taco".to_owned(), String::new()),
                ("tada".to_owned(), "remove".to_owned())
            ],
            "the user's own says so, and counts as used once now"
        );
        app.update(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(reactions(&app).len(), 1, "nothing changed");
        keys(&mut app, "+");
        assert_eq!(
            names(&app),
            ["taco", "tada", "+1"],
            "used as often as +1 now, and Slack lists it first"
        );
    }

    #[test]
    fn someone_elses_reaction_is_joined_and_a_message_going_ends_the_choice() {
        let mut taco = reacted(&["U2"], 1);
        taco.emoji = Emoji::named("taco");
        let mut app = reacting_to(vec![taco]);
        keys(&mut app, "+taco");
        assert_eq!(choices(&app), [("taco".to_owned(), String::new())]);
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [react_request("taco", ReactionChange::Added)]
        );
        assert_eq!(reactions(&app), [("taco".to_owned(), 2, true)]);

        keys(&mut app, "+");
        app.update(slack(Update::Live(LiveEvent::Deleted {
            conversation: id("C2"),
            ts: seconds(5),
        })));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn picking_ones_own_reaction_takes_it_back_and_a_refusal_undoes_what_was_shown() {
        let mut thumbs = reacted(&["U2", "UME"], 2);
        thumbs.emoji = Emoji::named("+1");
        thumbs.emoji.skin_tone = Some(3);
        let mut app = reacting_to(vec![thumbs.clone()]);
        keys(&mut app, "++");
        assert_eq!(
            choices(&app),
            [("+1".to_owned(), "remove".to_owned())],
            "in any skin tone"
        );
        let commands = app.update(key(KeyCode::Tab));
        let request = ask(Request::React {
            conversation: id("C2"),
            ts: seconds(5),
            emoji: thumbs.emoji.clone(),
            change: ReactionChange::Removed,
        });
        assert_eq!(commands, [request]);
        assert_eq!(reactions(&app), [("+1".to_owned(), 1, false)]);

        app.update(slack(Update::Reacted {
            conversation: id("C2"),
            ts: seconds(5),
            emoji: thumbs.emoji,
            change: ReactionChange::Removed,
            result: Err(Error::Refused {
                method: "reactions.remove",
                code: "ratelimited".into(),
            }),
        }));
        assert_eq!(
            reactions(&app),
            [("+1".to_owned(), 2, true)],
            "back as it was"
        );
        assert_eq!(
            told(&app),
            Some("couldn't take back :+1:: slack refused reactions.remove: ratelimited")
        );
    }

    #[test]
    fn plus_needs_a_message() {
        let (mut app, _) = ready_app();
        keys(&mut app, "+");
        assert_eq!(app.mode, Mode::Normal, "the sidebar has none");
    }

    fn reacted(users: &[&str], count: u32) -> slackers_model::Reaction {
        slackers_model::Reaction {
            emoji: slackers_model::Emoji::named("wave"),
            users: users.iter().map(|user| UserId::new(*user)).collect(),
            count,
        }
    }

    fn link(url: &str, label: Option<&str>) -> Inline {
        Inline::Link {
            url: url.into(),
            label: label.map(str::to_owned),
        }
    }

    fn file(name: &str, link: Option<&str>) -> slackers_model::File {
        slackers_model::File {
            name: name.into(),
            mimetype: "application/pdf".into(),
            link: link.map(str::to_owned),
            image: None,
        }
    }

    fn details(app: &App) -> Option<&Details> {
        if let Mode::Details(details) = &app.mode {
            Some(details)
        } else {
            None
        }
    }

    #[test]
    fn k_shows_a_messages_details_and_asks_for_everyone_who_reacted_when_some_went_unnamed() {
        let mut app =
            reading_random_with(RichText::plain("hi"), Vec::new(), vec![reacted(&["U4"], 3)]);

        let commands = app.update(key(KeyCode::Char('K')));
        assert_eq!(
            details(&app).map(|details| (&details.place, details.ts)),
            Some((&place("C2", None), seconds(5)))
        );
        let reactions = ask(Request::Reactions {
            conversation: id("C2"),
            ts: seconds(5),
        });
        assert!(commands.contains(&reactions), "{commands:?}");
        assert!(commands.contains(&ask(Request::User(UserId::new("U4")))));

        let commands = app.update(slack(Update::Reactions {
            conversation: id("C2"),
            ts: seconds(5),
            result: Ok(vec![reacted(&["U4", "U5", "U6"], 3)]),
        }));
        let held = workspace(&app, W0).timelines[&id("C2")]
            .get(seconds(5))
            .unwrap();
        assert_eq!(held.reactions, [reacted(&["U4", "U5", "U6"], 3)]);
        assert_eq!(
            commands,
            [
                ask(Request::User(UserId::new("U5"))),
                ask(Request::User(UserId::new("U6")))
            ],
            "the names of those now listed"
        );

        app.update(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        let commands = app.update(key(KeyCode::Char('K')));
        assert!(!commands.contains(&reactions), "everyone is named now");

        app.update(slack(Update::Reactions {
            conversation: id("C2"),
            ts: seconds(5),
            result: Err(Error::SignedOut),
        }));
        assert!(told(&app).is_some(), "a failure is told");
    }

    #[test]
    fn the_details_list_where_a_message_leads_for_one_to_be_opened_or_copied() {
        let content = RichText {
            blocks: vec![slackers_model::Block::Paragraph(vec![
                link("https://docs.rs/ratatui", Some("docs")),
                link("https://docs.rs/ratatui", None),
                link("file:///etc/passwd", None),
                link("HTTP://example.com", None),
            ])],
        };
        let files = vec![
            file("a.pdf", Some("https://test.slack.com/files/U2/F1/a.pdf")),
            file("hidden", None),
        ];
        let mut app = reading_random_with(content, files, Vec::new());
        let held = workspace(&app, W0).timelines[&id("C2")]
            .get(seconds(5))
            .unwrap();
        let links: Vec<(LinkKind, String, String)> = workspace(&app, W0)
            .links(&id("C2"), held)
            .into_iter()
            .map(|link| (link.kind, link.label, link.url))
            .collect();
        let message_link = "https://test.slack.com/archives/C2/p5000000?thread_ts=5.000000&cid=C2";
        assert_eq!(
            links,
            [
                (
                    LinkKind::Web,
                    "docs".into(),
                    "https://docs.rs/ratatui".into()
                ),
                (
                    LinkKind::Web,
                    "HTTP://example.com".into(),
                    "HTTP://example.com".into()
                ),
                (
                    LinkKind::File,
                    "a.pdf".into(),
                    "https://test.slack.com/files/U2/F1/a.pdf".into()
                ),
                (
                    LinkKind::Message,
                    "this message in Slack".into(),
                    message_link.into()
                ),
            ],
            "each web link once, files Slack links to, then the Message, which starts a Thread"
        );

        app.update(key(KeyCode::Char('K')));
        app.update(key(KeyCode::Char('j')));
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [Command::Open("HTTP://example.com".into())]
        );
        assert_eq!(
            details(&app).map(|details| details.selected),
            Some(1),
            "the details stay up for another choice"
        );

        app.update(key(KeyCode::Char('k')));
        assert_eq!(
            app.update(key(KeyCode::Char('y'))),
            [Command::Copy("https://docs.rs/ratatui".into())]
        );
        assert!(details(&app).is_some(), "copying too");

        for _ in 0..5 {
            app.update(key(KeyCode::Char('j')));
        }
        assert_eq!(details(&app).unwrap().selected, 3, "stops at the last");
        assert!(app.update(key(KeyCode::Char('9'))).is_empty());
        assert_eq!(
            app.update(key(KeyCode::Char('3'))),
            [Command::Open(
                "https://test.slack.com/files/U2/F1/a.pdf".into()
            )]
        );
        assert!(details(&app).is_some());
        app.update(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);

        let one_link = RichText {
            blocks: vec![slackers_model::Block::Paragraph(vec![link(
                "https://a.example/",
                None,
            )])],
        };
        let mut app = reading_random_with(one_link, Vec::new(), Vec::new());
        app.update(key(KeyCode::Char('K')));
        app.update(key(KeyCode::Enter));
        assert!(
            details(&app).is_some(),
            "the Message's own link is still there to choose"
        );

        let mut app = reading_random_with(RichText::plain("no links"), Vec::new(), Vec::new());
        app.update(key(KeyCode::Char('K')));
        let only = "https://test.slack.com/archives/C2/p5000000?thread_ts=5.000000&cid=C2";
        assert_eq!(
            app.update(key(KeyCode::Enter)),
            [Command::Open(only.into())]
        );
        assert_eq!(
            app.mode,
            Mode::Normal,
            "taking the only choice closes the details"
        );
        app.update(key(KeyCode::Char('K')));
        assert_eq!(
            app.update(key(KeyCode::Char('y'))),
            [Command::Copy(only.into())]
        );
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn the_details_offer_an_apps_buttons_first_and_enter_presses_one() {
        let button = |label: &str, primary| slackers_model::Button {
            label: label.into(),
            primary,
            press: slackers_model::Press(label.to_lowercase()),
        };
        let (send, shuffle) = (button("Send GIF", true), button("Shuffle", false));
        let content = RichText {
            blocks: vec![
                Block::Paragraph(vec![link("https://giphy.com/cat", None)]),
                Block::Actions(vec![send.clone(), shuffle.clone()]),
            ],
        };
        let mut app = reading_random();
        let preview = Message {
            ephemeral: true,
            ..message(5, "U2", content)
        };
        app.update(slack(Update::Live(LiveEvent::Edited {
            conversation: id("C2"),
            message: preview,
        })));
        app.focus = Focus::Timeline;
        let held = workspace(&app, W0).timelines[&id("C2")]
            .get(seconds(5))
            .unwrap();
        assert_eq!(
            workspace(&app, W0).choices(&id("C2"), held),
            [
                Choice::Press(send.clone()),
                Choice::Press(shuffle.clone()),
                Choice::Open(Link {
                    kind: LinkKind::Web,
                    label: "https://giphy.com/cat".into(),
                    url: "https://giphy.com/cat".into(),
                }),
            ],
            "buttons, then links; Slack keeps no page for an ephemeral Message"
        );
        let press = |button: &slackers_model::Button| {
            ask(Request::Press {
                conversation: id("C2"),
                ts: seconds(5),
                button: button.clone(),
            })
        };

        app.update(key(KeyCode::Char('K')));
        assert_eq!(app.update(key(KeyCode::Enter)), [press(&send)]);
        assert!(
            details(&app).is_some(),
            "pressing leaves the others to choose"
        );

        assert!(app.update(key(KeyCode::Char('y'))).is_empty());
        assert!(details(&app).is_some(), "a button has nothing to copy");
        assert_eq!(app.update(key(KeyCode::Char('2'))), [press(&shuffle)]);

        app.update(key(KeyCode::Char('j')));
        app.update(key(KeyCode::Char('j')));
        assert_eq!(
            app.update(key(KeyCode::Char('y'))),
            [Command::Copy("https://giphy.com/cat".into())]
        );

        app.update(slack(Update::Pressed {
            conversation: id("C2"),
            ts: seconds(5),
            button: shuffle,
            result: Err(Error::Refused {
                method: "blocks.actions",
                code: "invalid_service_id".into(),
            }),
        }));
        assert_eq!(
            told(&app),
            Some("couldn't press Shuffle: slack refused blocks.actions: invalid_service_id")
        );
    }

    #[test]
    fn o_opens_a_messages_only_link_and_otherwise_shows_its_details_to_choose() {
        let linking = || RichText {
            blocks: vec![slackers_model::Block::Paragraph(vec![link(
                "https://a.example/",
                None,
            )])],
        };
        let mut app = reading_random_with(linking(), Vec::new(), Vec::new());
        assert_eq!(
            app.update(key(KeyCode::Char('o'))),
            [Command::Open("https://a.example/".into())]
        );
        assert_eq!(app.mode, Mode::Normal);

        let pdf = file("a.pdf", Some("https://test.slack.com/files/U2/F1/a.pdf"));
        let mut app = reading_random_with(linking(), vec![pdf], Vec::new());
        assert!(app.update(key(KeyCode::Char('o'))).is_empty());
        assert!(details(&app).is_some());
    }

    #[test]
    fn a_replys_details_link_to_its_thread_and_close_once_it_is_deleted() {
        let mut app = reading_random();
        app.focus = Focus::Timeline;
        app.update(key(KeyCode::Enter));
        app.update(slack(Update::Replies {
            conversation: id("C2"),
            parent: seconds(5),
            result: Ok(vec![message(5, "U2", RichText::plain("hi")), reply(6, 5)]),
        }));
        app.update(key(KeyCode::Char('K')));
        let shown = details(&app).unwrap();
        assert_eq!(
            (&shown.place, shown.ts),
            (&place("C2", Some(5)), seconds(6))
        );
        let held = workspace(&app, W0).threads[&(id("C2"), seconds(5))]
            .get(seconds(6))
            .unwrap();
        let links = workspace(&app, W0).links(&id("C2"), held);
        assert_eq!(
            links.last().map(|link| link.url.as_str()),
            Some("https://test.slack.com/archives/C2/p6000000?thread_ts=5.000000&cid=C2")
        );

        app.update(slack(Update::Live(LiveEvent::Deleted {
            conversation: id("C2"),
            ts: seconds(6),
        })));
        assert_eq!(app.mode, Mode::Normal);
    }
}
