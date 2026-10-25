//! slackers' domain model: Workspaces, Conversations, Messages and what's Unread in them, independent of how Slack
//! sends them and how the terminal shows them.
//!
//! The vocabulary is defined in `CONTEXT.md` at the repository root.

mod activity;
mod conversation;
mod emoji;
mod id;
mod message;
mod text;
mod timeline;
mod unread;

pub use activity::{ActivityItem, Happened, MentionOf, MessageRef};
pub use conversation::{Conversation, ConversationKind};
pub use emoji::{CustomEmoji, CustomEmojiSet, Emoji};
pub use id::{ConversationId, ParseTsError, Ts, UserId, WorkspaceId};
pub use message::{Author, File, Image, Message, Reaction, ThreadSummary};
pub use text::{Block, Broadcast, Button, Inline, Press, RichText, Style};
pub use timeline::{ReactionChange, Timeline};
pub use unread::Unread;
