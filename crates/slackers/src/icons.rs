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
/// `oct-image`: an image on its way.
pub(crate) const IMAGE: &str = "\u{f4e5} ";
/// `oct-link`: a link to a web page.
pub(crate) const LINK: &str = "\u{f44c} ";
/// `md-slack`: something on Slack's web client.
pub(crate) const SLACK: &str = "\u{f04b1} ";
/// `oct-paperclip`: a shared file.
pub(crate) const FILE: &str = "\u{f4fb} ";
/// `oct-pencil`: an edited Message.
pub(crate) const EDITED: &str = "\u{f448} ";
/// `oct-stack`: a Virtual Workspace.
pub(crate) const VIRTUAL_WORKSPACE: &str = "\u{f51e} ";
/// `oct-triangle_right`: a folded sidebar section.
pub(crate) const FOLDED: &str = "\u{f44a} ";
/// `oct-triangle_down`: an unfolded sidebar section.
pub(crate) const UNFOLDED: &str = "\u{f44b} ";
/// `oct-chevron_right`: between the parts of a pane's title, as in a breadcrumb.
pub(crate) const CRUMB: &str = "\u{f460} ";
/// `md-account_circle`: a person, until their avatar loads, or wherever it doesn't show.
pub(crate) const AVATAR: &str = "\u{f0009} ";
/// `oct-megaphone`: an `@here` or `@channel`.
pub(crate) const BROADCAST: &str = "\u{f45f} ";
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
