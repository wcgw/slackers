//! slackers in wcgw-kit (its D1): what slackers knows of the user's Workspaces, and what it does with it. The editor
//! around it (buffers, windows, keys, the command line, the composer, highlight groups, pictures and the mouse) is
//! the kit's; [`Slackers`] is the application it runs.
//!
//! Every pane is a buffer (the kit's D3): the sidebar, one per Conversation and per Thread opened, Activity, and a
//! page saying how signing in goes. slackers renders them from what it knows after every change, as blocks keyed by
//! what they show (a Message by its timestamp), so the kit's cursor stays on the Message it was on.
//!
//! A Conversation, or a Followed Thread, with something unread opens on its first unread Message, under a rule
//! saying what's new, loading older history to reach it; it's read once the cursor reaches its newest Message
//! while the terminal has focus. While it doesn't, a DM, a Message that mentions the user, or a Reply in one of
//! their Followed Threads shows a desktop notification.
//!
//! A Draft starting with `/` and a word runs that slash command rather than posting: `/` brings up the Workspace's
//! commands to choose from, as `@` does people and `:` emoji. Starting it with a space posts it as text instead.
//! When the command posts for the user elsewhere, as `/dm @someone hi` does, slackers follows it there.

mod commands;
mod compose;
mod navigate;
mod render;
mod slack;
mod workspace;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::SystemTime;

use jiff::tz::TimeZone;
use slackers_model::{Button, ConversationId, Message, Ts, UserId};
use slackers_slack::{Request, Update};
use wcgw_kit::BufferId;
use wcgw_kit::app::Quitting;
use wcgw_kit::doc::Document;
use wcgw_kit::shell::{CursorLine, Placement, Size, WindowId};

use crate::activity::Activity;
use crate::config::VirtualWorkspace;
use crate::draft::{References, Written};
use crate::sidebar::Sidebar;
use crate::typing::Typing;
use crate::{hl, icons};

pub(crate) use render::picture_of;
pub(crate) use workspace::{Phase, Workspace, Workspaces};

/// The kit slackers runs in.
pub(crate) type Kit = wcgw_kit::Kit<Slackers>;

/// What slackers' code returns for the runtime to do.
pub(crate) type Effects = wcgw_kit::Effects<Slackers>;

/// One thing for the runtime to do.
pub(crate) type Effect = wcgw_kit::Effect<Msg, Io>;

/// The width of the sidebar's window, in cells.
const SIDEBAR_WIDTH: u16 = 30;

/// The narrowest room beside the sidebar, in cells, that shows a Thread beside its Conversation rather than instead
/// of it.
const SPLIT_AT: u16 = 120;

/// The most pages of history loaded to reach the Message a Conversation opens on: its first unread, or the one an
/// Activity item is about.
const JUMP_PAGES: u32 = 10;

/// The most notifications held back until the names in them are known; past that, the oldest go.
const HELD_NOTIFICATIONS: usize = 20;

/// How long after running a slash command a Message the user posts elsewhere counts as the command's doing, in
/// microseconds.
const COMMAND_POSTS_WITHIN: u64 = 15 * 1_000_000;

/// How far Slack's clock, which times Messages, may be behind this one, in microseconds.
const CLOCK_SKEW: u64 = 5 * 1_000_000;

/// What slackers' own work tells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Msg {
    /// News from a Workspace's Session.
    Slack(WorkspaceKey, Update),
    /// Signing in to `workspace` can't even start, for `reason`: its credentials aren't in the keyring, say.
    CantSignIn {
        workspace: WorkspaceKey,
        reason: String,
    },
    /// Saving the Virtual Workspaces, as [`Io::SaveVirtualWorkspaces`] asked, failed for this reason.
    SaveFailed(String),
    /// A while has passed since Slack said, as `heard`, that `user` was typing at `place`.
    StoppedTyping {
        place: Place,
        user: UserId,
        heard: u64,
    },
    /// Draw again what a key changed, apart from the key: a window reading at the end of what changed follows it
    /// then, as it does what arrives from Slack, rather than staying where the key left it.
    Refresh,
}

/// What slackers asks its runtime to do: the kit's effects (the clipboard, the browser, notifications) are the
/// kit's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Io {
    /// Ask a Workspace's Session for something.
    Slack(WorkspaceKey, Request),
    /// Write these Virtual Workspaces to the config file in place of those there, keeping the rest of it, and report
    /// a failure with [`Msg::SaveFailed`].
    SaveVirtualWorkspaces(Vec<VirtualWorkspace>),
}

/// `request`, to the Workspace at `key`, as an effect.
fn slack(key: WorkspaceKey, request: Request) -> Effect {
    Effect::App(Io::Slack(key, request))
}

/// What text in slackers' buffers refers to: what `:open` (`<CR>`, a click) opens, and what commands act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// Activity, merged from every Workspace: the sidebar's first row.
    Activity,
    /// A section of the sidebar, by its header: opening it folds it, or unfolds it.
    Fold,
    /// A Conversation.
    Conversation(ConversationRef),
    /// A Followed Thread, by its Conversation and parent.
    Thread {
        conversation: ConversationRef,
        parent: Ts,
    },
    /// An Activity item, by its Workspace and key.
    ActivityItem(WorkspaceKey, String),
    /// A Message, where it's held: its Conversation, or the Thread whose buffer shows it.
    Message(Place, Ts),
    /// A web link.
    Url(String),
    /// Someone a Message mentions.
    Person(WorkspaceKey, UserId),
    /// Reacting to a Message with an emoji, or taking the user's reaction back, as the react picker offers it.
    React { place: Place, ts: Ts, emoji: String },
    /// Pressing a button an app put in a Message.
    Press {
        place: Place,
        ts: Ts,
        button: Button,
    },
}

/// A Workspace, by its place among those the user signs in to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WorkspaceKey(usize);

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

/// Where a Draft is written and a Message posted: a Conversation, or a Thread in it. Each has a buffer of its own
/// once opened.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Place {
    pub(crate) conversation: ConversationRef,
    /// The Thread, by its parent; `None` for the Conversation itself.
    pub(crate) thread: Option<Ts>,
}

/// What one of slackers' buffers shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shows {
    /// How signing in goes, until something is opened.
    SignIn,
    Sidebar,
    Activity,
    /// A Conversation's Messages, or a Thread's.
    Place(Place),
}

/// A Message on its way to Slack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outgoing {
    /// Matches the [`Request::Post`] to its [`Update::Posted`].
    id: u64,
    pub(crate) place: Place,
    pub(crate) written: Written,
}

/// An edit of one of the user's Messages, written in its Place's composer.
#[derive(Debug)]
struct Editing {
    /// The Message.
    ts: Ts,
    /// What was being written there before, set aside until the edit is saved or given up; `None` if nothing was.
    set_aside: Option<(String, References)>,
    started: Started,
}

/// Where an edit was started, which giving it up goes back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Started {
    /// In the composer, with `<Up>`: back to writing there.
    InComposer,
    /// On its Message, with `A`: back to Normal mode.
    OnMessage,
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

/// Everything slackers knows that the editor around it doesn't.
#[derive(Debug)]
pub(crate) struct Slackers {
    pub(crate) workspaces: Workspaces,
    /// The Virtual Workspaces, as the config defines them.
    pub(crate) virtual_workspaces: Vec<VirtualWorkspace>,
    sidebar: Sidebar,
    /// Every Workspace's Activity.
    activity: Activity,
    /// Who's typing where, in every Workspace.
    typing: Typing,
    /// What each of slackers' buffers shows.
    shows: HashMap<BufferId, Shows>,
    /// The buffer of each Place opened.
    places: HashMap<Place, BufferId>,
    sidebar_buffer: BufferId,
    sign_in_buffer: BufferId,
    activity_buffer: Option<BufferId>,
    /// The window Conversations open in, if the user hasn't closed it.
    main_window: WindowId,
    /// The sidebar's window, while it shows.
    sidebar_window: Option<WindowId>,
    /// The window beside the Conversation that Threads open in, on a screen wide enough.
    thread_window: Option<WindowId>,
    /// Where what's new starts, by Place: after this Message, as it was when the Place was opened.
    new_after: HashMap<Place, Ts>,
    /// Conversations loading older history to reach the Message they opened on, with how many more pages may load.
    reaching: HashMap<ConversationRef, (Ts, u32)>,
    /// Buffers whose windows wait for their cursor to land on a block, by its key, before following their end.
    landing: HashMap<BufferId, String>,
    /// What the labels written in each Place's composer stand for.
    references: HashMap<Place, References>,
    /// Messages posted but not yet confirmed, oldest first.
    outbox: Vec<Outgoing>,
    /// Slash commands run but not yet answered, by request id, to put back in their composers should they fail.
    commands_sent: Vec<(u64, Place, Written)>,
    /// The slash command run last, while it may yet post for the user elsewhere: slackers follows it there.
    following: Option<Following>,
    /// A Conversation the user marked unread: it stays so until they open one.
    kept_unread: Option<ConversationRef>,
    /// Edits being written, by where: in that Place's composer, what was there set aside meanwhile.
    editing: HashMap<Place, Editing>,
    /// Edits saved but not yet confirmed, to put back should Slack refuse them.
    edits_sent: Vec<(Place, Ts, Written)>,
    next_post: u64,
    /// Whether the terminal has focus. Messages count as read only while it does, and notifications show only while
    /// it doesn't.
    focused: bool,
    /// Messages to notify of once the names of everyone in them are known, by Workspace and Conversation, oldest
    /// first; at most [`HELD_NOTIFICATIONS`].
    held_notifications: Vec<(WorkspaceKey, ConversationId, Message)>,
    /// The Message whose details the hover card shows, while it does.
    details: Option<(Place, Ts)>,
    /// The name an @-mention being written has, by Workspace, as the composer last said: what to have Slack find
    /// people by once it answers for a shorter one.
    mentioning: Option<(WorkspaceKey, String)>,
    /// Whether slackers quits once what's on its way to Slack is there, as `:qa` asked.
    quitting: bool,
    /// Where the user is, for the times Messages show.
    zone: TimeZone,
    /// Whether the sidebar's cursor goes to the first row with something unread, as Workspaces sign in: until the
    /// user does anything.
    suggesting: bool,
    /// The sidebar's first row with something unread, as last drawn.
    first_unread: Option<usize>,
    /// The width each buffer was last laid out for, to lay it out again when its window's changes.
    widths: HashMap<BufferId, u16>,
}

impl Slackers {
    /// slackers signing in to the Workspaces with these `labels`, in this order, and listing `virtual_workspaces`, in
    /// the editor it runs in: the sidebar on the left, and beside it the page saying how signing in goes.
    pub(crate) fn session(
        labels: Vec<String>,
        virtual_workspaces: Vec<VirtualWorkspace>,
    ) -> (Self, Kit) {
        let mut kit = Kit::new("slackers", Document::new(), false);
        *kit.theme_mut() = hl::theme();
        for (name, nerd, plain) in icons::ICONS {
            kit.define_icon(name, nerd, plain);
        }
        commands::install(&mut kit);
        let sign_in_buffer = kit.current_buffer();
        let main_window = kit.current_window();
        let sidebar_buffer = kit.add_buffer("sidebar", Document::new(), false);
        kit.buffer_mut(sidebar_buffer)
            .set_cursorline(CursorLine::Row);
        let sidebar_window = kit.open_window(
            Placement::FarLeft,
            Size::Cells(SIDEBAR_WIDTH),
            sidebar_buffer,
        );
        let mut app = Self {
            workspaces: Workspaces::new(labels),
            virtual_workspaces,
            sidebar: Sidebar::default(),
            activity: Activity::default(),
            typing: Typing::default(),
            shows: HashMap::from([
                (sign_in_buffer, Shows::SignIn),
                (sidebar_buffer, Shows::Sidebar),
            ]),
            places: HashMap::new(),
            sidebar_buffer,
            sign_in_buffer,
            activity_buffer: None,
            main_window,
            sidebar_window: Some(sidebar_window),
            thread_window: None,
            new_after: HashMap::new(),
            reaching: HashMap::new(),
            landing: HashMap::new(),
            references: HashMap::new(),
            outbox: Vec::new(),
            commands_sent: Vec::new(),
            following: None,
            kept_unread: None,
            editing: HashMap::new(),
            edits_sent: Vec::new(),
            next_post: 0,
            focused: true,
            held_notifications: Vec::new(),
            details: None,
            mentioning: None,
            quitting: false,
            zone: TimeZone::system(),
            suggesting: true,
            first_unread: None,
            widths: HashMap::new(),
        };
        app.list_sidebar();
        // Nothing is signed in to yet: nothing to ask for.
        let _ = app.refresh(&mut kit);
        (app, kit)
    }

    /// Every Workspace, signed in to or not, in order.
    pub(crate) fn workspace_keys(&self) -> impl Iterator<Item = WorkspaceKey> + use<> {
        (0..self.workspaces.len()).map(WorkspaceKey)
    }

    /// The Message posted at `ts`, as `place` holds it, and its Workspace.
    pub(crate) fn message(&self, place: &Place, ts: Ts) -> Option<(&Workspace, &Message)> {
        let workspace = self.workspaces.get(place.conversation.workspace)?;
        Some((workspace, workspace.timeline(place)?.get(ts)?))
    }

    /// The Place buffer `buffer` shows, if it shows one.
    fn place_of(&self, buffer: BufferId) -> Option<&Place> {
        match self.shows.get(&buffer)? {
            Shows::Place(place) => Some(place),
            Shows::SignIn | Shows::Sidebar | Shows::Activity => None,
        }
    }

    /// Draws every buffer a window shows afresh from what slackers knows, and the sidebar; what it asks Slack for
    /// to show them, such as the Messages Activity items are about.
    fn refresh(&mut self, kit: &mut Kit) -> Effects {
        let mut effects = Vec::new();
        self.render_sidebar(kit);
        let mut shown: Vec<BufferId> = kit
            .window_ids()
            .into_iter()
            .filter_map(|window| kit.window_buffer(window))
            .collect();
        shown.sort_unstable();
        shown.dedup();
        for buffer in shown {
            effects.extend(self.render(kit, buffer));
        }
        effects
    }

    /// Draws `buffer` afresh, unless it's the sidebar, which [`Slackers::refresh`] draws anyway.
    fn render(&mut self, kit: &mut Kit, buffer: BufferId) -> Effects {
        match self.shows.get(&buffer).cloned() {
            Some(Shows::SignIn) => {
                self.render_sign_in(kit);
                Vec::new()
            }
            Some(Shows::Activity) => self.render_activity(kit, buffer),
            Some(Shows::Place(place)) => {
                self.render_place(kit, buffer, &place);
                Vec::new()
            }
            Some(Shows::Sidebar) | None => Vec::new(),
        }
    }

    /// What follows any change: Messages read are marked so, pictures and names asked for, and the buffers shown
    /// drawn again. `reading` are the Places whose newest Message the user was reading before the change.
    fn changed(&mut self, kit: &mut Kit, reading: &[Place]) -> Effects {
        let mut effects = self.mark_read(reading);
        // Replies come and go unread, and the open Thread stays listed: keep the Threads section current.
        let threads = self.followed_items(kit);
        self.sidebar.set(&crate::sidebar::Section::Threads, threads);
        effects.extend(self.refresh(kit));
        effects.extend(self.refresh_details(kit));
        self.suggest_in_sidebar(kit);
        effects
    }

    /// Puts the sidebar's cursor on its first row with something unread, without opening it (opening reads it,
    /// which the user should choose), until the user does anything.
    fn suggest_in_sidebar(&self, kit: &mut Kit) {
        let (true, Some(row), Some(window)) =
            (self.suggesting, self.first_unread, self.sidebar_window(kit))
        else {
            return;
        };
        let layout = wcgw_kit::doc::Layout::new(kit.buffer(self.sidebar_buffer).doc(), u16::MAX);
        let cursor = wcgw_kit::doc::Cursor::at_row(&layout, row);
        kit.set_cursor(window, cursor);
    }

    /// Lays out again the buffers whose windows changed width since they were last laid out, as a `:vsplit` does.
    fn rewrap(&mut self, kit: &mut Kit) {
        let changed: Vec<BufferId> = self
            .widths
            .iter()
            .filter(|&(&buffer, &width)| {
                !kit.windows_showing(buffer).is_empty() && kit.text_width(buffer) != width
            })
            .map(|(&buffer, _)| buffer)
            .collect();
        for buffer in changed {
            // Nothing to ask for that wasn't asked for when it was last drawn.
            let _ = self.render(kit, buffer);
        }
    }
}

impl wcgw_kit::App for Slackers {
    type Target = Target;
    type Msg = Msg;
    type Effect = Io;

    fn update(&mut self, kit: &mut Kit, msg: Msg) -> Effects {
        let reading = self.reading_newest(kit);
        let mut effects = match msg {
            Msg::Slack(workspace, update) => self.slack(kit, workspace, update),
            Msg::CantSignIn { workspace, reason } => {
                self.workspaces.set_phase(workspace, Phase::Failed(reason));
                Vec::new()
            }
            Msg::SaveFailed(reason) => {
                kit.error(format!("couldn't save the virtual workspaces: {reason}"));
                Vec::new()
            }
            Msg::StoppedTyping { place, user, heard } => {
                self.typing.stopped(&place, &user, heard);
                Vec::new()
            }
            Msg::Refresh => Vec::new(),
        };
        effects.extend(self.changed(kit, &reading));
        effects.extend(self.quit_when_sent(kit));
        effects
    }

    /// `:q` on the last window, or `:qa`: at once, unless Messages, edits or slash commands are on their way to
    /// Slack, which slackers waits for; `:qa!` doesn't.
    fn quit(&mut self, kit: &mut Kit) -> Quitting {
        let sending = self.sending();
        if sending == 0 {
            return Quitting::Now;
        }
        self.quitting = true;
        let what = if sending == 1 {
            "what you sent".to_owned()
        } else {
            format!("the {sending} things you sent")
        };
        kit.info(format!("quitting once Slack has {what}; :qa! quits now"));
        Quitting::Later
    }

    /// What the text under the cursor refers to: a row of the sidebar opens its Conversation, Thread or Activity; a
    /// Message, its Thread; a link, the browser; an Activity item, its Message.
    fn open(&mut self, kit: &mut Kit, target: Target) -> Result<Effects, String> {
        let mut effects = self.open_target(kit, target)?;
        // A key or a click opens: what it reads, `keyed` marks, after.
        effects.extend(self.changed(kit, &[]));
        Ok(effects)
    }

    /// `:e {name}`: the Conversation of that name, as its buffer would be called.
    fn edit(&mut self, kit: &mut Kit, name: &str) -> Result<Effects, String> {
        let conversation = self
            .sidebar
            .conversations()
            .into_iter()
            .find(|conversation| self.buffer_name(&Place::of(conversation)) == name)
            .cloned()
            .ok_or_else(|| format!("E94: No matching buffer for {name}"))?;
        self.open(kit, Target::Conversation(conversation))
    }

    fn edit_names(&self, _kit: &Kit) -> Vec<String> {
        self.sidebar
            .conversations()
            .into_iter()
            .map(|conversation| self.buffer_name(&Place::of(conversation)))
            .collect()
    }

    /// `<C-l>`: asks Slack again for what `buffer` shows.
    fn reload(&mut self, kit: &mut Kit, buffer: BufferId) -> Option<Effects> {
        let mut effects = match self.shows.get(&buffer)?.clone() {
            Shows::Place(place) => self.load(&place, true),
            Shows::Activity => self.refresh_activity_everywhere(),
            Shows::SignIn | Shows::Sidebar => return None,
        };
        effects.extend(self.refresh(kit));
        Some(effects)
    }

    /// A window shows `buffer` now: asks for what it shows, unless it's loaded or on its way, and draws it.
    fn shown(&mut self, kit: &mut Kit, buffer: BufferId) -> Effects {
        let mut effects = match self.shows.get(&buffer).cloned() {
            Some(Shows::Place(place)) => self.load(&place, false),
            Some(Shows::Activity) if self.activity.entries().is_empty() => {
                self.refresh_activity_everywhere()
            }
            _ => Vec::new(),
        };
        effects.extend(self.render(kit, buffer));
        effects
    }

    /// After every key: the cursor may have reached the newest Message, or the oldest loaded; a window may have
    /// changed width; a composer may need its title changed.
    fn keyed(&mut self, kit: &mut Kit) -> Effects {
        self.suggesting = false;
        self.rewrap(kit);
        let mut effects = self.keep_composers(kit);
        effects.extend(self.load_older_at_top(kit));
        effects.extend(self.load_older_activity(kit));
        let reading = self.reading_newest(kit);
        let marked = self.mark_read(&reading);
        if !marked.is_empty() {
            // Not the buffers: what a key changes there, the kit wouldn't follow.
            self.render_sidebar(kit);
        }
        effects.extend(marked);
        effects
    }

    /// The terminal is `width` cells wide now: a Thread goes beside its Conversation, or instead of it, and
    /// Messages are laid out again to the windows' widths.
    fn resized(&mut self, kit: &mut Kit, _width: u16, _height: u16) -> Effects {
        self.reshape(kit);
        self.refresh(kit)
    }

    fn focus(&mut self, kit: &mut Kit, focused: bool) -> Effects {
        self.focused = focused;
        if !focused {
            return Vec::new();
        }
        // Back in front of the user: notifications still held back are moot.
        self.held_notifications.clear();
        let reading = self.reading_newest(kit);
        self.changed(kit, &reading)
    }
}

impl Place {
    /// `conversation` itself, rather than one of its Threads.
    pub(crate) fn of(conversation: &ConversationRef) -> Self {
        Self {
            conversation: conversation.clone(),
            thread: None,
        }
    }
}
