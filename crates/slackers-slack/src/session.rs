use std::sync::Arc;

use slackers_model::{
    Activity, Conversation, ConversationId, CustomEmojiSet, Emoji, Message, ReactionChange, Ts,
    UserId, WorkspaceId,
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
    /// Posts a top-level Message as the user; answered by [`Update::Posted`].
    Post {
        /// Where.
        conversation: ConversationId,
        /// The Thread to reply in, by its parent; `None` to post at the top level.
        thread: Option<Ts>,
        /// What, as typed: `mrkdwn` markup such as `*bold*` and `:emoji:` applies, but `&`, `<` and `>` are
        /// taken literally.
        text: String,
        /// Any number the caller picks, to match the answer to the request.
        id: u64,
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

/// Something that just happened in a Workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveEvent {
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
    let gateway = snapshot.workspace.clone();
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
    tasks.spawn(socket::follow(
        credentials,
        gateway,
        me.clone(),
        Arc::clone(&notify),
    ));

    while let Some(request) = requests.recv().await {
        let (api, notify, me) = (Arc::clone(&api), Arc::clone(&notify), me.clone());
        tasks.spawn(async move { serve(&api, request, &me, &notify).await });
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

async fn serve(api: &Api, request: Request, me: &UserId, notify: &Notify) {
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
    }
}
