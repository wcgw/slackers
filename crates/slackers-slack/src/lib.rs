//! slackers' Slack adapter: signs in to a Workspace with browser session credentials and talks to Slack the way
//! its web client does (see `docs/adr/0001-slack-session-token-auth.md`).
//!
//! A [`Session`] runs in the background. Ask it for things with [`Session::request`]; everything it learns
//! arrives as an [`Update`] in the model's terms. Nothing about Slack's wire formats leaks out of this crate.

mod api;
mod credentials;
mod error;
mod mrkdwn;
mod session;
mod socket;
mod web;
mod wire;

pub use credentials::Credentials;
pub use error::Error;
pub use session::{
    ActivityCursor, ActivityPage, Connection, EmojiList, FollowedThread, HistoryPage, LiveEvent,
    Part, Person, Request, Session, Snapshot, Update, User,
};
pub use web::Permalinks;
