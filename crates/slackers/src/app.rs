//! The app's state, and how events change it. No I/O here: every effect is a [`Command`] for the runtime to carry
//! out, so that `update` can be tested on its own.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use slackers_gfx::{Fit, Picture};
use slackers_model::{
    Activity, Author, Conversation, ConversationId, ConversationKind, Inline, Message, Timeline,
    UserId,
};
use slackers_slack::{Connection, Error, LiveEvent, Request, Snapshot, Update};

use crate::emoji::Emojis;

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
    Quit,
}

/// Which pane keys go to.
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

/// Everything slackers shows. Only [`App::update`] changes it; the view reads it.
#[derive(Debug)]
pub(crate) struct App {
    /// The name the user gave the Workspace, until Slack names it.
    pub(crate) label: String,
    pub(crate) phase: Phase,
    pub(crate) connection: Option<Connection>,
    /// The latest problem worth telling the user about.
    pub(crate) notice: Option<String>,
    /// People's names, as far as they've been looked up.
    pub(crate) users: HashMap<UserId, String>,
    requested_users: HashSet<UserId>,
    pub(crate) emojis: Emojis,
    /// The sidebar entry under the cursor.
    pub(crate) selected: usize,
    pub(crate) open: Option<ConversationId>,
    pub(crate) focus: Focus,
    /// How many lines the open Timeline is scrolled up from its newest Message.
    pub(crate) scroll: usize,
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
            selected: 0,
            open: None,
            focus: Focus::Sidebar,
            scroll: 0,
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

    /// Reacts to `event`, returning what the runtime should do.
    pub(crate) fn update(&mut self, event: Event) -> Vec<Command> {
        let mut commands = Vec::new();
        match event {
            Event::Terminal(TerminalEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                self.key(key, &mut commands);
            }
            Event::Terminal(_) => {}
            Event::Slack(update) => self.slack(update, &mut commands),
            Event::PictureReady { url, picture } => self.emojis.loaded(url, picture),
            Event::PictureFailed { url } => self.emojis.failed(url),
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
        commands
    }

    fn key(&mut self, key: KeyEvent, commands: &mut Vec<Command>) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, control) {
            (KeyCode::Char('q'), false) | (KeyCode::Char('c'), true) => {
                return commands.push(Command::Quit);
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
                    self.scroll_by(self.half_page(), commands)
                }
                (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => {
                    self.scroll_by(-self.half_page(), commands)
                }
                (KeyCode::Char('k') | KeyCode::Up, _) => self.scroll_by(1, commands),
                (KeyCode::Char('j') | KeyCode::Down, _) => self.scroll_by(-1, commands),
                (KeyCode::Char('g') | KeyCode::Home, _) => self.scroll_by(isize::MAX, commands),
                (KeyCode::Char('G') | KeyCode::End, _) => self.scroll = 0,
                (KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc, _) => {
                    self.focus = Focus::Sidebar
                }
                _ => {}
            },
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
                        workspace
                            .timelines
                            .entry(conversation)
                            .or_default()
                            .add_history(page.messages, page.has_more);
                        self.want_users(people, commands);
                    }
                    Err(error) => self.notice = Some(describe(&error)),
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
        // Start on the first Conversation with something unread, if any.
        let first = conversations
            .iter()
            .position(|(_, activity)| activity.has_unread())
            .unwrap_or(0);
        let activity = conversations
            .iter()
            .map(|(c, a)| (c.id.clone(), a.clone()))
            .collect();
        let workspace = Workspace {
            name: snapshot.name,
            me: snapshot.me,
            conversations: conversations.into_iter().map(|(c, _)| c).collect(),
            activity,
            timelines: HashMap::new(),
            loading: HashSet::new(),
        };
        let first_id = workspace.conversations.get(first).map(|c| c.id.clone());
        self.phase = Phase::Ready(Box::new(workspace));
        self.want_users(partners, commands);
        self.selected = first;
        if let Some(id) = first_id {
            self.open(id, commands);
        }
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

    #[test]
    fn signing_in_sorts_the_sidebar_and_opens_the_first_unread() {
        let (app, commands) = ready_app();
        let names: Vec<&str> = app
            .workspace()
            .unwrap()
            .conversations
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(names, ["C1", "C2", "D1"], "channels by name, then DMs");
        assert_eq!(app.selected, 1);
        assert_eq!(app.open, Some(ConversationId::new("C2")));
        assert_eq!(
            commands,
            [
                Command::Slack(Request::User(UserId::new("U9"))),
                Command::Slack(Request::History {
                    conversation: ConversationId::new("C2"),
                    before: None
                }),
            ]
        );
    }

    #[test]
    fn history_fills_the_timeline_and_asks_for_names_once() {
        let (mut app, _) = ready_app();
        let page = HistoryPage {
            messages: vec![
                message(3, "U2", RichText::plain("hi")),
                message(2, "U2", RichText::plain("hello")),
            ],
            has_more: true,
        };
        let commands = app.update(Event::Slack(Update::History {
            conversation: ConversationId::new("C2"),
            result: Ok(page),
        }));
        assert_eq!(commands, [Command::Slack(Request::User(UserId::new("U2")))]);
        assert_eq!(
            app.workspace().unwrap().timelines[&ConversationId::new("C2")]
                .messages()
                .len(),
            2
        );
    }

    #[test]
    fn scrolling_to_the_top_loads_older_history_once() {
        let (mut app, _) = ready_app();
        let page = HistoryPage {
            messages: vec![message(3, "U2", RichText::plain("hi"))],
            has_more: true,
        };
        app.update(Event::Slack(Update::History {
            conversation: ConversationId::new("C2"),
            result: Ok(page),
        }));
        app.focus = Focus::Timeline;
        app.update(Event::Drawn(Drawn {
            max_scroll: 5,
            page: 10,
            ..Drawn::default()
        }));

        let key = |code| Event::Terminal(TerminalEvent::Key(KeyEvent::from(code)));
        assert!(app.update(key(KeyCode::Char('k'))).is_empty());
        let commands = app.update(key(KeyCode::Char('g')));
        assert_eq!(app.scroll, 5);
        assert_eq!(
            commands,
            [Command::Slack(Request::History {
                conversation: ConversationId::new("C2"),
                before: Some(seconds(3))
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
                conversation: ConversationId::new(conversation),
                message,
            }))
        };
        app.update(post("C1", message(10, "UME", RichText::plain("mine"))));
        let mut reply = message(11, "U2", RichText::plain("reply"));
        reply.reply_to = Some(seconds(1));
        app.update(post("C1", reply));
        let activity = |app: &App, id: &str| {
            app.workspace().unwrap().activity[&ConversationId::new(id)].clone()
        };
        assert!(!activity(&app, "C1").has_unread());

        app.update(post("C1", message(12, "U2", mention)));
        app.update(post("D1", message(13, "U9", RichText::plain("dm"))));
        assert_eq!(activity(&app, "C1").mentions(), 1);
        assert_eq!(activity(&app, "D1").mentions(), 1, "DMs count as mentions");

        app.update(Event::Slack(Update::Live(LiveEvent::Marked {
            conversation: ConversationId::new("C1"),
            up_to: seconds(12),
        })));
        assert!(!activity(&app, "C1").has_unread());
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

    #[test]
    fn q_and_ctrl_c_quit() {
        let (mut app, _) = ready_app();
        assert_eq!(
            app.update(Event::Terminal(TerminalEvent::Key(KeyEvent::from(
                KeyCode::Char('q')
            )))),
            [Command::Quit]
        );
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            app.update(Event::Terminal(TerminalEvent::Key(ctrl_c))),
            [Command::Quit]
        );
        assert!(
            app.update(Event::Terminal(TerminalEvent::Key(KeyEvent::from(
                KeyCode::Char('c')
            ))))
            .is_empty()
        );
    }
}
