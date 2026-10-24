use crate::{Emoji, Inline, RichText, Ts, UserId};

/// A Message as slackers shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// When it was posted, which is also its id within its Conversation.
    pub ts: Ts,
    /// Who posted it.
    pub author: Author,
    /// What it says.
    pub content: RichText,
    /// The Thread this Message is a Reply in, by its parent's [`Ts`]; `None` for a top-level Message.
    pub reply_to: Option<Ts>,
    /// Its Replies, when it starts a Thread.
    pub thread: Option<ThreadSummary>,
    /// Reactions, in the order they were first added.
    pub reactions: Vec<Reaction>,
    /// Files shared with it, in the order they were attached.
    pub files: Vec<File>,
    /// Whether it was changed after it was posted.
    pub edited: bool,
}

/// Who posted a Message.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Author {
    /// A person, or a bot user, of the Workspace.
    User(UserId),
    /// An app or integration posting under its own name.
    Bot {
        /// The name it posts under.
        name: String,
        /// Where its avatar image is; `None` when it has none.
        avatar: Option<String>,
    },
}

/// The Replies to a Message that starts a Thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadSummary {
    /// How many Replies there are; at least one.
    pub replies: u32,
    /// When the newest Reply was posted.
    pub latest: Ts,
}

/// One emoji's reactions to a Message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    /// What people reacted with.
    pub emoji: Emoji,
    /// How many people reacted with it; at least one.
    pub count: u32,
    /// Whether the user is one of them.
    pub mine: bool,
}

/// A file shared in a Message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Its file name, as uploaded.
    pub name: String,
    /// Its media type, such as `image/png`; empty when Slack didn't say.
    pub mimetype: String,
}

impl Message {
    /// Whether it @-mentions `user` by name.
    pub fn mentions(&self, user: &UserId) -> bool {
        self.content.mentions(user)
    }

    /// Whether `user` wrote it.
    pub fn is_by(&self, user: &UserId) -> bool {
        matches!(&self.author, Author::User(id) if id == user)
    }

    /// Every emoji it shows, in its text and in its reactions.
    pub fn emoji(&self) -> impl Iterator<Item = &Emoji> {
        let in_text = self.content.inlines().filter_map(|inline| match inline {
            Inline::Emoji(emoji) => Some(emoji),
            _ => None,
        });
        in_text.chain(self.reactions.iter().map(|reaction| &reaction.emoji))
    }
}
