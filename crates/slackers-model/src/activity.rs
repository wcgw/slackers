//! Activity: what happens in a Workspace that concerns the user.

use crate::{ConversationId, Emoji, Ts, UserId};

/// An item of a Workspace's Activity: something about one Message that concerns the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityItem {
    /// Slack's name for it, unique within its Workspace. An item that changes, such as a Followed Thread's as
    /// Replies come, keeps it.
    pub key: String,
    /// When it last changed.
    pub at: Ts,
    /// Whether the user has yet to see it.
    pub unread: bool,
    /// The Message it's about: the one that mentions the user, the one reacted to, a Followed Thread's newest Reply,
    /// a DM's newest Message, or the one that added the user to a Channel.
    pub message: MessageRef,
    /// What happened to, or in, the Message.
    pub happened: Happened,
}

/// Where a Message is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MessageRef {
    /// The Conversation it's in.
    pub conversation: ConversationId,
    /// It, within that Conversation.
    pub ts: Ts,
    /// Its Thread's parent, when it's a Reply and that's known.
    pub thread: Option<Ts>,
}

/// What happened, in an [`ActivityItem`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Happened {
    /// Someone mentioned the user in the Message.
    Mention {
        /// Who.
        author: UserId,
        /// How it names the user.
        of: MentionOf,
    },
    /// Someone reacted to the Message, which is the user's.
    Reaction {
        /// Who.
        user: UserId,
        /// With what.
        emoji: Emoji,
    },
    /// Replies came in a Followed Thread; the Message is the newest.
    Replies {
        /// How many of them are unread.
        unread: u32,
    },
    /// A Message came in a DM.
    Dm,
    /// Someone added the user to the Message's Channel.
    Added {
        /// Who.
        author: UserId,
    },
}

/// How a Mention names the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionOf {
    /// By name.
    User,
    /// Through a user group they're in.
    Group,
    /// As everyone in the Conversation: `@here`, `@channel` or `@everyone`.
    Everyone,
}
