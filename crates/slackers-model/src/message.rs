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
    /// Whether only the user sees it, as an app's answer to them. Slack doesn't keep it: history never has it.
    pub ephemeral: bool,
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
    /// Who reacted with it, in the order Slack lists them. A Message's history names only the first 50 people, so
    /// this may leave some out: see [`Reaction::unnamed`].
    pub users: Vec<UserId>,
    /// How many people reacted with it; at least one, and never fewer than `users` names.
    pub count: u32,
}

/// A file shared in a Message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Its file name, as uploaded.
    pub name: String,
    /// Its media type, such as `image/png`; empty when Slack didn't say.
    pub mimetype: String,
    /// Where it opens in a browser; `None` when Slack gave no link, as for a file hidden from the user.
    pub link: Option<String>,
    /// How to show it inline; `None` unless it's an image Slack made a rendition of.
    pub image: Option<Image>,
}

/// An image to show inline: where to fetch it, and how big it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Where it is.
    pub url: String,
    /// Its width, in pixels.
    pub width: u32,
    /// Its height, in pixels.
    pub height: u32,
}

impl Reaction {
    /// How many of the people who reacted [`Reaction::users`] leaves out.
    pub fn unnamed(&self) -> u32 {
        let named = u32::try_from(self.users.len()).unwrap_or(u32::MAX);
        self.count.saturating_sub(named)
    }
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
