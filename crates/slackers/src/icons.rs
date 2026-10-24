//! Nerd Font icons (Octicons and Material Design, as named in Nerd Fonts' `glyphnames.json`).
//!
//! They're Private Use Area characters that a Nerd Font may draw wider than one cell, so each is followed by a space
//! for it to spill into. Codepoints were checked against Nerd Fonts 3's glyph list.

/// `oct-hash`: a public Channel.
pub(crate) const CHANNEL: &str = "\u{f4df} ";
/// `oct-lock`: a private Channel.
pub(crate) const PRIVATE_CHANNEL: &str = "\u{f456} ";
/// `oct-person`: a DM.
pub(crate) const DM: &str = "\u{f415} ";
/// `oct-people`: a group DM.
pub(crate) const GROUP_DM: &str = "\u{f4fd} ";
/// `oct-mention`: Mentions waiting.
pub(crate) const MENTION: &str = "\u{f486} ";
/// `oct-dot_fill`: unread Messages waiting.
pub(crate) const UNREAD: &str = "\u{f444} ";
/// `oct-comment_discussion`: a Thread's Replies.
pub(crate) const THREAD: &str = "\u{f442} ";
/// `oct-paperclip`: a shared file.
pub(crate) const FILE: &str = "\u{f4fb} ";
/// `oct-pencil`: an edited Message.
pub(crate) const EDITED: &str = "\u{f448} ";
/// `fa-slack`: the Workspace.
pub(crate) const WORKSPACE: &str = "\u{f198} ";
/// `md-account_circle`: a person, until their avatar loads.
pub(crate) const AVATAR: &str = "\u{f0009} ";
/// `md-robot`: a bot, until its avatar loads.
pub(crate) const BOT: &str = "\u{f06a9} ";
/// `md-keyboard`: Insert mode.
pub(crate) const INSERT: &str = "\u{f030c} ";
/// `oct-clock`: a Message on its way to Slack.
pub(crate) const SENDING: &str = "\u{f43a} ";
/// `md-lan_connect`: live updates flowing.
pub(crate) const LIVE: &str = "\u{f0318} ";
/// `md-lan_disconnect`: live updates interrupted.
pub(crate) const OFFLINE: &str = "\u{f0319} ";
/// `oct-sync`: waiting on Slack.
pub(crate) const LOADING: &str = "\u{f46a} ";
/// `oct-alert`: something went wrong.
pub(crate) const ALERT: &str = "\u{f421} ";
