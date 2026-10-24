use crate::{ConversationId, UserId};

/// A Conversation the user takes part in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    /// Its id within its Workspace.
    pub id: ConversationId,
    /// Channel, DM or group DM.
    pub kind: ConversationKind,
    /// What to call it: a Channel's name without the `#`, or the handles of a group DM's members joined by `, `.
    /// Empty for a DM, which is named after the person in [`ConversationKind::Dm`].
    pub name: String,
}

/// What kind of Conversation it is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConversationKind {
    /// A Channel.
    Channel {
        /// Whether it's invite-only.
        private: bool,
    },
    /// A DM between the user and one other person.
    Dm {
        /// The other person.
        with: UserId,
    },
    /// A DM between the user and several other people.
    GroupDm,
}
