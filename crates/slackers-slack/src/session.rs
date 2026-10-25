use std::collections::HashSet;
use std::sync::Arc;

use slackers_model::{
    Activity, Broadcast, Conversation, ConversationId, CustomEmojiSet, Emoji, Message,
    ReactionChange, Ts, UserId, WorkspaceId,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::api::Api;
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
    /// The Workspace's Custom Emoji. Arrives once, some time after [`Update::Ready`].
    CustomEmoji(CustomEmojiSet),
    /// The answer to [`Request::History`].
    History {
        /// Whose history was asked for.
        conversation: ConversationId,
        /// The page, or why there isn't one.
        result: Result<HistoryPage, Error>,
    },
    /// The answer to [`Request::User`].
    User(User),
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
    /// The Conversations in the user's sidebar, with their Activity, in no particular order.
    pub conversations: Vec<(Conversation, Activity)>,
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
    pub activity: Activity,
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
    /// Where their avatar image is, about 48 pixels square; `None` when they have none.
    pub avatar: Option<String>,
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
}

/// Something that just happened in a Workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
    /// A Conversation the [`Snapshot`] didn't list appeared in the user's sidebar: they joined a Channel, or someone
    /// started a DM. It arrives before any Message posted there.
    Joined {
        /// Which.
        conversation: Conversation,
        /// Its Activity up to its Read Marker; Messages that brought it arrive after.
        activity: Activity,
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
        /// Whether it was the user.
        mine: bool,
        /// Added or taken back.
        change: ReactionChange,
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
    let me = snapshot.me.clone();
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
            match api.custom_emoji().await {
                Ok(set) => notify(Update::CustomEmoji(set)),
                Err(error) => tracing::warn!(%error, "couldn't list custom emoji"),
            }
        });
    }
    let (heard, hearing) = mpsc::unbounded_channel();
    tasks.spawn(socket::follow(
        credentials,
        workspace.clone(),
        me.clone(),
        heard,
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
        let (api, notify) = (Arc::clone(&api), Arc::clone(&notify));
        let (me, workspace) = (me.clone(), workspace.clone());
        tasks.spawn(async move { serve(&api, request, &me, &workspace, &notify).await });
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
    Found: Future<Output = Result<Option<(Conversation, Activity)>, Error>>,
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
                Ok(Some((conversation, activity))) => {
                    known.insert(conversation.id.clone());
                    notify(Update::Live(LiveEvent::Joined {
                        conversation,
                        activity,
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

/// Answers `request`, made of the Workspace `workspace`, signed in to as `me`.
async fn serve(api: &Api, request: Request, me: &UserId, workspace: &WorkspaceId, notify: &Notify) {
    match request {
        Request::History {
            conversation,
            before,
        } => {
            let result = api.history(&conversation, before, me).await;
            notify(Update::History {
                conversation,
                result,
            });
        }
        Request::User(id) => match api.user(&id).await {
            Ok(user) => notify(Update::User(user)),
            Err(error) => tracing::warn!(%error, user = %id, "couldn't look up a user"),
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
            let result = api.post(&conversation, thread, &text, me).await;
            notify(Update::Posted {
                id,
                conversation,
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
            let result = api.replies(&conversation, parent, me).await;
            notify(Update::Replies {
                conversation,
                parent,
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
                        Activity::default(),
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
