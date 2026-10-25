use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use slackers_model::{
    ActivityItem, BotId, Broadcast, Button, Conversation, ConversationId, CustomEmojiSet, Emoji,
    Message, Presence, Reaction, ReactionChange, RichText, Ts, Unread, UserId, WorkspaceId,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use crate::api::Api;
use crate::web::Permalinks;
use crate::{Credentials, Error, socket};

/// A signed-in Workspace, followed by a background task.
///
/// The task signs in, keeps a live connection to Slack, and answers [`Request`]s. Everything it learns goes to the
/// callback given to [`Session::start`]. It stops when the `Session` and all its clones are dropped.
#[derive(Clone, Debug)]
pub struct Session {
    requests: mpsc::UnboundedSender<Request>,
}

/// Something to ask a [`Session`] for. Each answer arrives as an [`Update`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// A page of a Conversation's history; answered by [`Update::History`].
    History {
        /// Whose history.
        conversation: ConversationId,
        /// Only Messages posted before this; `None` for the newest ones.
        before: Option<Ts>,
    },
    /// A person's name; answered by [`Update::User`], or not at all if Slack doesn't know them.
    User(UserId),
    /// A bot's name; answered by [`Update::Bot`], or not at all if Slack doesn't know it.
    Bot(BotId),
    /// Follows whether these people are using Slack, instead of whoever was followed before; answered by
    /// [`LiveEvent::Presence`], soon for each of them and again whenever that changes.
    FollowPresence(Vec<UserId>),
    /// A file, image, avatar or Custom Emoji; answered by [`Update::Downloaded`].
    Download {
        /// Where it is. Requests to Slack's own hosts carry the session's credentials, so private files work;
        /// requests elsewhere carry none.
        url: String,
    },
    /// Posts a Message as the user; answered by [`Update::Posted`].
    Post {
        /// Where.
        conversation: ConversationId,
        /// The Thread to reply in, by its parent; `None` to post at the top level.
        thread: Option<Ts>,
        /// What, in order.
        text: Vec<Part>,
        /// Any number the caller picks, to match the answer to the request.
        id: u64,
    },
    /// The people in the Workspace whose names match `query`, best first, as Slack's own people search finds
    /// them; answered by [`Update::FoundPeople`].
    FindPeople {
        /// What they're called, or the start of it.
        query: String,
    },
    /// Adds the user's reaction to a Message, or takes it back; answered by [`Update::Reacted`].
    React {
        /// Where.
        conversation: ConversationId,
        /// To which Message.
        ts: Ts,
        /// With what.
        emoji: Emoji,
        /// Added or taken back.
        change: ReactionChange,
    },
    /// Presses one of the buttons an app put in a Message; answered by [`Update::Pressed`]. What it does, such as
    /// changing the Message, arrives live.
    Press {
        /// Where.
        conversation: ConversationId,
        /// In which Message.
        ts: Ts,
        /// Which.
        button: Button,
    },
    /// Every reaction to a Message, naming everyone who reacted, where its history names only the first 50
    /// people of each; answered by [`Update::Reactions`].
    Reactions {
        /// Where.
        conversation: ConversationId,
        /// Which Message.
        ts: Ts,
    },
    /// Runs a slash command, as if typed in the Conversation or Thread; answered by [`Update::RanCommand`]. What it
    /// does, such as posting or replying for the user's eyes only, arrives live.
    RunCommand {
        /// Where.
        conversation: ConversationId,
        /// The Thread it's run in, by its parent; `None` for the Conversation.
        thread: Option<Ts>,
        /// Which, `/` included.
        command: String,
        /// What follows it, in order.
        text: Vec<Part>,
        /// Any number the caller picks, to match the answer to the request.
        id: u64,
    },
    /// Replaces the text of one of the user's Messages; answered by [`Update::Edited`].
    Edit {
        /// Where.
        conversation: ConversationId,
        /// Which.
        ts: Ts,
        /// Its new text, in order.
        text: Vec<Part>,
    },
    /// A page of the user's Activity, newest first; answered by [`Update::Activity`].
    Activity {
        /// Where the page starts: `None` for the newest, else as the page before said.
        cursor: Option<ActivityCursor>,
    },
    /// One Message, a Reply or not; answered by [`Update::Message`].
    Message {
        /// Where.
        conversation: ConversationId,
        /// Which.
        ts: Ts,
    },
    /// A Thread, parent and Replies; answered by [`Update::Replies`].
    Replies {
        /// Where.
        conversation: ConversationId,
        /// The Thread's parent.
        parent: Ts,
    },
    /// Moves the user's Read Marker. Slack confirms with a [`LiveEvent::Marked`]; failures are only logged.
    Mark {
        /// Where.
        conversation: ConversationId,
        /// The newest Message read.
        up_to: Ts,
    },
    /// Moves the user's Read Marker in a Followed Thread. Slack confirms with a [`LiveEvent::ThreadMarked`];
    /// failures are only logged.
    MarkThread {
        /// Where.
        conversation: ConversationId,
        /// The Thread's parent.
        parent: Ts,
        /// The newest Reply read.
        up_to: Ts,
    },
}

/// What a [`Session`] learned.
#[derive(Debug)]
pub enum Update {
    /// Signed in. Always the first Update unless [`Update::SignInFailed`] is.
    Ready(Box<Snapshot>),
    /// Couldn't sign in. The last Update: the Session has stopped.
    SignInFailed(Error),
    /// The Workspace's emoji. Arrives once, some time after [`Update::Ready`].
    Emoji(EmojiList),
    /// How many times the user used each emoji, by name, as Slack's clients count it. Arrives once, some time after
    /// [`Update::Ready`].
    EmojiUse(HashMap<String, u32>),
    /// The answer to [`Request::History`].
    History {
        /// Whose history was asked for.
        conversation: ConversationId,
        /// The page, or why there isn't one.
        result: Result<HistoryPage, Error>,
    },
    /// The answer to [`Request::User`].
    User(User),
    /// The answer to [`Request::Bot`].
    Bot(Bot),
    /// The answer to [`Request::Replies`].
    Replies {
        /// Where.
        conversation: ConversationId,
        /// The Thread's parent.
        parent: Ts,
        /// The parent and its Replies, oldest first, or why there are none.
        result: Result<Vec<Message>, Error>,
    },
    /// The answer to [`Request::Post`].
    Posted {
        /// The request's `id`.
        id: u64,
        /// Where it was posted.
        conversation: ConversationId,
        /// The Message as Slack stored it, or why it wasn't posted.
        result: Result<Message, Error>,
    },
    /// The answer to [`Request::FindPeople`].
    FoundPeople {
        /// What was asked for.
        query: String,
        /// Who, best first, or why no one.
        result: Result<Vec<Person>, Error>,
    },
    /// The Workspace's slash commands. Arrives once, some time after [`Update::Ready`].
    Commands(Vec<SlashCommand>),
    /// The answer to [`Request::RunCommand`].
    RanCommand {
        /// The request's `id`.
        id: u64,
        /// What Slack replied for the user's eyes only, if anything, or why the command didn't run.
        result: Result<Option<RichText>, Error>,
    },
    /// The answer to [`Request::Edit`].
    Edited {
        /// Where.
        conversation: ConversationId,
        /// Which.
        ts: Ts,
        /// The Message's new content, as Slack stored it, or why it wasn't edited.
        result: Result<RichText, Error>,
    },
    /// The answer to [`Request::Activity`].
    Activity {
        /// Where the page asked for starts.
        cursor: Option<ActivityCursor>,
        /// The page, or why there isn't one.
        result: Result<ActivityPage, Error>,
    },
    /// The answer to [`Request::Message`].
    Message {
        /// Where.
        conversation: ConversationId,
        /// Which.
        ts: Ts,
        /// The Message; `None` if it's gone, or why it isn't known.
        result: Result<Option<Message>, Error>,
    },
    /// The answer to [`Request::React`]: done, or why not. A reaction that was as asked already counts as done.
    Reacted {
        /// Where.
        conversation: ConversationId,
        /// To which Message.
        ts: Ts,
        /// With what.
        emoji: Emoji,
        /// Added or taken back.
        change: ReactionChange,
        /// Why it wasn't done, if it wasn't.
        result: Result<(), Error>,
    },
    /// The answer to [`Request::Press`]: done, or why not.
    Pressed {
        /// Where.
        conversation: ConversationId,
        /// In which Message.
        ts: Ts,
        /// Which.
        button: Button,
        /// Why it wasn't pressed, if it wasn't.
        result: Result<(), Error>,
    },
    /// The answer to [`Request::Reactions`].
    Reactions {
        /// Where.
        conversation: ConversationId,
        /// Which Message.
        ts: Ts,
        /// Its reactions, or why they aren't known.
        result: Result<Vec<Reaction>, Error>,
    },
    /// The answer to [`Request::Download`].
    Downloaded {
        /// What was asked for.
        url: String,
        /// The bytes, or why there are none.
        result: Result<Vec<u8>, Error>,
    },
    /// Something just happened in the Workspace.
    Live(LiveEvent),
    /// The live connection came up or dropped.
    Connection(Connection),
}

/// The user's view of a Workspace when signing in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The Workspace's id, which is the org's for an Enterprise Grid session.
    pub workspace: WorkspaceId,
    /// The Workspace's name, as Slack shows it.
    pub name: String,
    /// Where the Workspace's icon is, about 44 pixels square; `None` when it uses Slack's default.
    pub icon: Option<String>,
    /// The signed-in user.
    pub me: UserId,
    /// Builds links to the Workspace's Messages.
    pub permalinks: Permalinks,
    /// The Conversations in the user's sidebar, with what's Unread in each, in no particular order.
    pub conversations: Vec<(Conversation, Unread)>,
    /// The Followed Threads with the latest Replies; possibly not all of them.
    pub followed_threads: Vec<FollowedThread>,
}

/// A Followed Thread, as signing in finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowedThread {
    /// Where.
    pub conversation: ConversationId,
    /// The Message that started it.
    pub parent: Message,
    /// Its Replies past its Read Marker. Mentions in them aren't counted: Slack only counts them across every
    /// Thread.
    pub unread: Unread,
}

/// A page of a Conversation's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPage {
    /// The Messages, newest first.
    pub messages: Vec<Message>,
    /// Whether older Messages exist.
    pub has_more: bool,
}

/// A person or bot in the Workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// Who.
    pub id: UserId,
    /// What to call them: their display name, else their full name, else their handle.
    pub name: String,
    /// Their full name; empty when they gave none.
    pub full_name: String,
    /// What they do, as their profile says, such as their job title; empty when it says nothing.
    pub title: String,
    /// Their time zone's IANA name, such as `Europe/Paris`; `None` when Slack doesn't say.
    pub time_zone: Option<String>,
    /// Where their avatar image is, about 48 pixels square; `None` when they have none.
    pub avatar: Option<String>,
}

/// A bot, which an app or integration posts through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bot {
    /// Which.
    pub id: BotId,
    /// What it's called, such as `giphy`.
    pub name: String,
    /// Where its avatar image is, about 48 pixels square; `None` when it has none.
    pub avatar: Option<String>,
}

/// A slash command a Workspace offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    /// What's typed to run it, `/` included.
    pub name: String,
    /// What it does, in a few words; may be empty.
    pub description: String,
    /// What it takes after its name, such as `[text]`; may be empty.
    pub usage: String,
}

/// A page of a Workspace's Activity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityPage {
    /// Its items, newest first.
    pub items: Vec<ActivityItem>,
    /// Where the next, older page starts; `None` after the last.
    pub next: Option<ActivityCursor>,
}

/// Where a page of Activity starts, as Slack says: what [`ActivityPage::next`] holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityCursor(pub String);

/// The emoji a Workspace has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmojiList {
    /// Those it defines itself.
    pub custom: CustomEmojiSet,
    /// Standard emoji, each by the name Slack lists it under, most common first; empty when Slack didn't list them.
    /// Slack knows some by other names too, such as `thumbsup` for `+1`.
    pub standard: Vec<String>,
}

/// Someone [`Request::FindPeople`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// Who.
    pub id: UserId,
    /// What to call them, as [`User::name`] does.
    pub name: String,
    /// Their full name; empty when they gave none.
    pub full_name: String,
}

/// A part of a Message to post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Text as typed: `mrkdwn` markup such as `*bold*` and `:emoji:` applies, but `&`, `<` and `>` are taken
    /// literally.
    Text(String),
    /// An @-mention of a person.
    User(UserId),
    /// An `@here`, `@channel` or `@everyone`.
    Broadcast(Broadcast),
    /// An @-mention of a user group, by its id.
    UserGroup(String),
    /// A link to a Channel.
    Channel(ConversationId),
    /// A link that shows as `label`.
    Link {
        /// Where it goes.
        url: String,
        /// What shows instead of `url`.
        label: String,
    },
}

/// Something that just happened in a Workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// A Conversation the [`Snapshot`] didn't list appeared in the user's sidebar: they joined a Channel, or someone
    /// started a DM. It arrives before any Message posted there.
    Joined {
        /// Which.
        conversation: Conversation,
        /// What's Unread in it, as of its Read Marker; the Messages that made it appear arrive after.
        unread: Unread,
    },
    /// A Message was posted. A Reply carries [`Message::reply_to`].
    Posted {
        /// Where.
        conversation: ConversationId,
        /// What.
        message: Message,
    },
    /// A Message or a Reply was edited; it replaces the one with the same [`Message::ts`].
    Edited {
        /// Where.
        conversation: ConversationId,
        /// Its new content.
        message: Message,
    },
    /// A Message was deleted.
    Deleted {
        /// Where.
        conversation: ConversationId,
        /// Which.
        ts: Ts,
    },
    /// Someone reacted to a Message, or took their reaction back.
    Reacted {
        /// Where.
        conversation: ConversationId,
        /// To which Message.
        ts: Ts,
        /// With what.
        emoji: Emoji,
        /// Who.
        user: UserId,
        /// Who wrote the Message, when Slack says.
        author: Option<UserId>,
        /// Added or taken back.
        change: ReactionChange,
    },
    /// Someone is typing, as their client says every few seconds while they do; nothing says when they stop.
    Typing {
        /// Where.
        conversation: ConversationId,
        /// The Thread, by its parent; `None` for the Conversation itself.
        thread: Option<Ts>,
        /// Who.
        user: UserId,
    },
    /// Whether some of the people followed with [`Request::FollowPresence`] are using Slack.
    Presence {
        /// Who.
        people: Vec<UserId>,
        /// Whether they are.
        presence: Presence,
    },
    /// The user's Read Marker moved, in this or another client.
    Marked {
        /// Where.
        conversation: ConversationId,
        /// The new Read Marker.
        up_to: Ts,
    },
    /// The user follows a Thread now: they started it, replied to it, or chose to.
    ThreadFollowed {
        /// Where.
        conversation: ConversationId,
        /// The Thread's parent.
        parent: Ts,
        /// Its Read Marker.
        read_up_to: Ts,
    },
    /// The user no longer follows a Thread.
    ThreadUnfollowed {
        /// Where.
        conversation: ConversationId,
        /// The Thread's parent.
        parent: Ts,
    },
    /// The user's Read Marker in a Followed Thread moved, in this or another client.
    ThreadMarked {
        /// Where.
        conversation: ConversationId,
        /// The Thread's parent.
        parent: Ts,
        /// The new Read Marker.
        up_to: Ts,
    },
}

/// The state of a [`Session`]'s live connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    /// Events are flowing.
    Live,
    /// The connection dropped; this is the `attempt`-th try to bring it back, after a growing pause.
    Reconnecting {
        /// How many tries so far, starting at 1.
        attempt: u32,
    },
}

/// The callback that receives a Session's [`Update`]s.
pub(crate) type Notify = Arc<dyn Fn(Update) + Send + Sync>;

/// What the live socket heard, before the Session makes sure the app knows each Conversation it's about.
#[derive(Debug)]
pub(crate) enum Heard {
    Event(LiveEvent),
    /// A Conversation appeared in the user's sidebar: they joined a Channel, or a DM opened.
    Appeared(ConversationId),
}

impl Session {
    /// Starts signing in with `credentials`, on a background task of the current Tokio runtime.
    ///
    /// `on_update` receives every [`Update`], starting with [`Update::Ready`] or [`Update::SignInFailed`]. It's
    /// called on the Session's tasks, possibly from several threads, and must not block.
    ///
    /// # Panics
    ///
    /// If called outside a Tokio runtime.
    pub fn start(
        credentials: Credentials,
        on_update: impl Fn(Update) + Send + Sync + 'static,
    ) -> Self {
        let (requests, received) = mpsc::unbounded_channel();
        tokio::spawn(run(credentials, received, Arc::new(on_update)));
        Self { requests }
    }

    /// Asks for something; the answer arrives as an [`Update`]. Does nothing once the Session has stopped, which
    /// [`Update::SignInFailed`] announces.
    pub fn request(&self, request: Request) {
        // Sending fails only when the task has stopped, which the caller learned from SignInFailed.
        let _ = self.requests.send(request);
    }
}

/// The Session's task: signs in, then follows live events and serves requests until every `Session` is dropped.
async fn run(
    credentials: Credentials,
    mut requests: mpsc::UnboundedReceiver<Request>,
    notify: Notify,
) {
    let api = match Api::new(credentials.clone()) {
        Ok(api) => Arc::new(api),
        Err(error) => return notify(Update::SignInFailed(error)),
    };
    let snapshot = match api.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => return notify(Update::SignInFailed(error)),
    };
    let workspace = snapshot.workspace.clone();
    let known = snapshot
        .conversations
        .iter()
        .map(|(conversation, _)| conversation.id.clone())
        .collect();
    notify(Update::Ready(Box::new(snapshot)));

    let mut tasks = JoinSet::new();
    {
        let (api, notify) = (Arc::clone(&api), Arc::clone(&notify));
        tasks.spawn(async move {
            match api.emoji().await {
                Ok(list) => notify(Update::Emoji(list)),
                Err(error) => tracing::warn!(%error, "couldn't list emoji"),
            }
            match api.emoji_use().await {
                Ok(used) => notify(Update::EmojiUse(used)),
                Err(error) => tracing::warn!(%error, "couldn't tell which emoji are used most"),
            }
            match api.commands().await {
                Ok(commands) => notify(Update::Commands(commands)),
                Err(error) => tracing::warn!(%error, "couldn't list slash commands"),
            }
        });
    }
    let (heard, hearing) = mpsc::unbounded_channel();
    let (presence, followed) = watch::channel(Vec::new());
    tasks.spawn(socket::follow(
        credentials,
        workspace.clone(),
        heard,
        followed,
        Arc::clone(&notify),
    ));
    let look_up = {
        let api = Arc::clone(&api);
        move |conversation: ConversationId| {
            let api = Arc::clone(&api);
            async move { api.conversation(&conversation).await }
        }
    };
    tasks.spawn(relay(known, look_up, hearing, Arc::clone(&notify)));

    while let Some(request) = requests.recv().await {
        if let Request::FollowPresence(people) = request {
            // The socket subscribes to them, and again whenever it reconnects.
            presence.send_replace(people);
            continue;
        }
        let (api, notify) = (Arc::clone(&api), Arc::clone(&notify));
        let workspace = workspace.clone();
        tasks.spawn(async move { serve(&api, request, &workspace, &notify).await });
        while let Some(finished) = tasks.try_join_next() {
            if let Err(error) = finished
                && error.is_panic()
            {
                tracing::error!(%error, "a slack task panicked");
            }
        }
    }
    // Every Session was dropped: stop following the Workspace.
    tasks.abort_all();
}

/// Passes on what the socket heard, introducing each Conversation the app doesn't know with [`LiveEvent::Joined`]
/// before anything posted in it. `known` holds those it knows; `look_up` finds one as the sidebar lists it, or says
/// the sidebar wouldn't (`None`). One that couldn't be looked up is tried again when something is next posted in it.
/// Runs until the socket stops hearing.
async fn relay<F, Found>(
    mut known: HashSet<ConversationId>,
    look_up: F,
    mut heard: mpsc::UnboundedReceiver<Heard>,
    notify: Notify,
) where
    F: Fn(ConversationId) -> Found,
    Found: Future<Output = Result<Option<(Conversation, Unread)>, Error>>,
{
    while let Some(item) = heard.recv().await {
        let (conversation, event) = match item {
            Heard::Appeared(conversation) => (conversation, None),
            Heard::Event(event) => match &event {
                LiveEvent::Posted { conversation, .. } => (conversation.clone(), Some(event)),
                _ => {
                    notify(Update::Live(event));
                    continue;
                }
            },
        };
        if !known.contains(&conversation) {
            // Wait for it, so that the app hears of the Conversation before anything else about it.
            match look_up(conversation.clone()).await {
                Ok(Some((conversation, unread))) => {
                    known.insert(conversation.id.clone());
                    notify(Update::Live(LiveEvent::Joined {
                        conversation,
                        unread,
                    }));
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, %conversation, "couldn't look up a conversation that appeared");
                }
            }
        }
        if let Some(event) = event {
            notify(Update::Live(event));
        }
    }
}

/// Answers `request`, made of the Workspace `workspace`.
async fn serve(api: &Api, request: Request, workspace: &WorkspaceId, notify: &Notify) {
    match request {
        Request::History {
            conversation,
            before,
        } => {
            let result = api.history(&conversation, before).await;
            notify(Update::History {
                conversation,
                result,
            });
        }
        Request::User(id) => match api.user(&id).await {
            Ok(user) => notify(Update::User(user)),
            Err(error) => tracing::warn!(%error, user = %id, "couldn't look up a user"),
        },
        Request::FollowPresence(_) => unreachable!("run hands it to the socket"),
        Request::Bot(id) => match api.bot(&id).await {
            Ok(bot) => notify(Update::Bot(bot)),
            Err(error) => tracing::warn!(%error, bot = %id, "couldn't look up a bot"),
        },
        Request::Download { url } => {
            let result = api.download(&url).await;
            notify(Update::Downloaded { url, result });
        }
        Request::Post {
            conversation,
            thread,
            text,
            id,
        } => {
            let result = api.post(&conversation, thread, &text).await;
            notify(Update::Posted {
                id,
                conversation,
                result,
            });
        }
        Request::RunCommand {
            conversation,
            thread,
            command,
            text,
            id,
        } => {
            let result = api
                .run_command(&conversation, thread, &command, &text)
                .await;
            notify(Update::RanCommand { id, result });
        }
        Request::Edit {
            conversation,
            ts,
            text,
        } => {
            let result = api.edit(&conversation, ts, &text).await;
            notify(Update::Edited {
                conversation,
                ts,
                result,
            });
        }
        Request::FindPeople { query } => {
            let result = api.find_people(workspace, &query).await;
            notify(Update::FoundPeople { query, result });
        }
        Request::Replies {
            conversation,
            parent,
        } => {
            let result = api.replies(&conversation, parent).await;
            notify(Update::Replies {
                conversation,
                parent,
                result,
            });
        }
        Request::Activity { cursor } => {
            let result = api.activity(cursor.as_ref()).await;
            notify(Update::Activity { cursor, result });
        }
        Request::Message { conversation, ts } => {
            let result = api.message(&conversation, ts).await;
            notify(Update::Message {
                conversation,
                ts,
                result,
            });
        }
        Request::React {
            conversation,
            ts,
            emoji,
            change,
        } => {
            let result = api.react(&conversation, ts, &emoji, change).await;
            notify(Update::Reacted {
                conversation,
                ts,
                emoji,
                change,
                result,
            });
        }
        Request::Press {
            conversation,
            ts,
            button,
        } => {
            let result = api.press(&conversation, ts, &button).await;
            notify(Update::Pressed {
                conversation,
                ts,
                button,
                result,
            });
        }
        Request::Reactions { conversation, ts } => {
            let result = api.reactions(&conversation, ts).await;
            notify(Update::Reactions {
                conversation,
                ts,
                result,
            });
        }
        Request::Mark {
            conversation,
            up_to,
        } => {
            if let Err(error) = api.mark(&conversation, up_to).await {
                tracing::warn!(%error, %conversation, "couldn't move the read marker");
            }
        }
        Request::MarkThread {
            conversation,
            parent,
            up_to,
        } => {
            if let Err(error) = api.mark_thread(&conversation, parent, up_to).await {
                tracing::warn!(%error, %conversation, "couldn't move a thread's read marker");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use slackers_model::{Author, ConversationKind, RichText};

    use super::*;

    fn id(id: &str) -> ConversationId {
        ConversationId::new(id)
    }

    fn posted(conversation: &str, seconds: u64) -> Heard {
        Heard::Event(LiveEvent::Posted {
            conversation: id(conversation),
            message: Message {
                ts: Ts::from_micros(seconds * 1_000_000),
                author: Author::User(UserId::new("U2")),
                content: RichText::plain("hi"),
                reply_to: None,
                thread: None,
                reactions: Vec::new(),
                files: Vec::new(),
                edited: false,
                ephemeral: false,
            },
        })
    }

    /// What the relay passes on, as `joined X`, `posted in X` or `other`, given what the socket `heard` and the
    /// Conversations `known`. Looking up `D9` finds a DM; `C8` isn't the user's; `C7` fails.
    async fn relayed(known: &[&str], heard: Vec<Heard>) -> (Vec<String>, Vec<String>) {
        let updates = Arc::new(Mutex::new(Vec::new()));
        let looked_up = Arc::new(Mutex::new(Vec::new()));
        let notify: Notify = {
            let updates = Arc::clone(&updates);
            Arc::new(move |update| {
                let described = match update {
                    Update::Live(LiveEvent::Joined { conversation, .. }) => {
                        format!("joined {}", conversation.id)
                    }
                    Update::Live(LiveEvent::Posted { conversation, .. }) => {
                        format!("posted in {conversation}")
                    }
                    _ => "other".to_owned(),
                };
                updates.lock().unwrap().push(described);
            })
        };
        let look_up = {
            let looked_up = Arc::clone(&looked_up);
            move |conversation: ConversationId| {
                looked_up.lock().unwrap().push(conversation.to_string());
                let found = match conversation.as_str() {
                    "D9" => Ok(Some((
                        Conversation {
                            id: conversation,
                            kind: ConversationKind::Dm {
                                with: UserId::new("U2"),
                            },
                            name: String::new(),
                        },
                        Unread::default(),
                    ))),
                    "C8" => Ok(None),
                    _ => Err(Error::refused("conversations.info", "channel_not_found")),
                };
                std::future::ready(found)
            }
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        for item in heard {
            sender.send(item).unwrap();
        }
        drop(sender);
        let known = known.iter().map(|known| id(known)).collect();
        relay(known, look_up, receiver, notify).await;
        let updates = updates.lock().unwrap().clone();
        let looked_up = looked_up.lock().unwrap().clone();
        (updates, looked_up)
    }

    #[tokio::test]
    async fn a_new_conversation_is_introduced_before_what_was_posted_in_it() {
        let marked = Heard::Event(LiveEvent::Marked {
            conversation: id("D9"),
            up_to: Ts::default(),
        });
        let (updates, looked_up) = relayed(
            &["C1"],
            vec![posted("C1", 1), marked, posted("D9", 2), posted("D9", 3)],
        )
        .await;
        assert_eq!(
            updates,
            [
                "posted in C1",
                "other",
                "joined D9",
                "posted in D9",
                "posted in D9"
            ]
        );
        assert_eq!(looked_up, ["D9"], "once, and only for what's posted");
    }

    #[tokio::test]
    async fn an_appearing_conversation_is_introduced_once() {
        let (updates, looked_up) = relayed(
            &["C1"],
            vec![
                Heard::Appeared(id("D9")),
                Heard::Appeared(id("D9")),
                Heard::Appeared(id("C1")),
            ],
        )
        .await;
        assert_eq!(updates, ["joined D9"]);
        assert_eq!(looked_up, ["D9"]);
    }

    #[tokio::test]
    async fn what_isnt_the_users_is_passed_on_and_failures_are_tried_again() {
        let (updates, looked_up) =
            relayed(&[], vec![posted("C8", 1), posted("C7", 2), posted("C7", 3)]).await;
        assert_eq!(updates, ["posted in C8", "posted in C7", "posted in C7"]);
        assert_eq!(looked_up, ["C8", "C7", "C7"]);
    }
}
