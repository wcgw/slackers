//! slackers' icons (wcgw-kit's D9): Nerd Font glyphs (Octicons and Material Design, as named in Nerd Fonts'
//! `glyphnames.json`), each with a plain one for terminals without them. The kit draws either in two cells, so
//! `:set icons=` never shifts a column.

/// `oct-hash`: a public Channel.
pub(crate) const CHANNEL: &str = "SlackersChannel";
/// `oct-lock`: a private Channel.
pub(crate) const PRIVATE_CHANNEL: &str = "SlackersPrivateChannel";
/// `oct-person`: a DM.
pub(crate) const DM: &str = "SlackersDm";
/// `oct-person_fill`: a DM with someone using Slack right now.
pub(crate) const DM_ACTIVE: &str = "SlackersDmActive";
/// `oct-people`: a group DM.
pub(crate) const GROUP_DM: &str = "SlackersGroupDm";
/// `oct-bell`: Activity.
pub(crate) const ACTIVITY: &str = "SlackersActivity";
/// `oct-smiley`: a reaction whose emoji can't be drawn.
pub(crate) const REACTION: &str = "SlackersReaction";
/// `oct-person_add`: being added to a Channel.
pub(crate) const ADDED: &str = "SlackersAdded";
/// `oct-terminal`: a slash command.
pub(crate) const COMMAND: &str = "SlackersCommand";
/// `oct-mention`: Mentions waiting.
pub(crate) const MENTION: &str = "SlackersMention";
/// `oct-dot_fill`: unread Messages waiting.
pub(crate) const UNREAD: &str = "SlackersUnread";
/// `oct-comment_discussion`: a Thread's Replies.
pub(crate) const THREAD: &str = "SlackersThread";
/// `oct-image`: an image.
pub(crate) const IMAGE: &str = "SlackersImage";
/// `oct-link`: a link to a web page.
pub(crate) const LINK: &str = "SlackersLink";
/// `md-slack`: something on Slack's web client.
pub(crate) const SLACK: &str = "SlackersSlack";
/// `oct-paperclip`: a shared file.
pub(crate) const FILE: &str = "SlackersFile";
/// `oct-pencil`: an edited Message.
pub(crate) const EDITED: &str = "SlackersEdited";
/// `oct-eye`: a Message only the user sees.
pub(crate) const EPHEMERAL: &str = "SlackersEphemeral";
/// `oct-stack`: a Virtual Workspace.
pub(crate) const VIRTUAL_WORKSPACE: &str = "SlackersVirtualWorkspace";
/// `oct-triangle_right`: a folded sidebar section.
pub(crate) const FOLDED: &str = "SlackersFolded";
/// `oct-triangle_down`: an unfolded sidebar section.
pub(crate) const UNFOLDED: &str = "SlackersUnfolded";
/// `oct-chevron_right`: between the parts of a window's title, as in a breadcrumb.
pub(crate) const CRUMB: &str = "SlackersCrumb";
/// `md-account_circle`: a person, without an avatar.
pub(crate) const AVATAR: &str = "SlackersAvatar";
/// `oct-megaphone`: an `@here` or `@channel`.
pub(crate) const BROADCAST: &str = "SlackersBroadcast";
/// `md-robot`: a bot, without an avatar.
pub(crate) const BOT: &str = "SlackersBot";
/// `oct-clock`: a Message on its way to Slack.
pub(crate) const SENDING: &str = "SlackersSending";
/// `md-lan_connect`: live updates flowing.
pub(crate) const LIVE: &str = "SlackersLive";
/// `md-lan_disconnect`: live updates interrupted.
pub(crate) const OFFLINE: &str = "SlackersOffline";
/// `oct-sync`: waiting on Slack.
pub(crate) const LOADING: &str = "SlackersLoading";
/// `oct-alert`: something went wrong.
pub(crate) const ALERT: &str = "SlackersAlert";
/// `oct-check`: signed in.
pub(crate) const SIGNED_IN: &str = "SlackersSignedIn";

/// Every icon: its name, its Nerd Font glyph, and its plain one, a single cell or nothing.
pub(crate) const ICONS: [(&str, &str, &str); 31] = [
    (CHANNEL, "\u{f4df}", "#"),
    (PRIVATE_CHANNEL, "\u{f456}", "*"),
    (DM, "\u{f415}", "@"),
    (DM_ACTIVE, "\u{f4ff}", "●"),
    (GROUP_DM, "\u{f4fd}", "&"),
    (ACTIVITY, "\u{f49a}", "!"),
    (REACTION, "\u{f4a2}", "+"),
    (ADDED, "\u{f4fe}", "+"),
    (COMMAND, "\u{f489}", "/"),
    (MENTION, "\u{f486}", "@"),
    (UNREAD, "\u{f444}", "•"),
    (THREAD, "\u{f442}", "↳"),
    (IMAGE, "\u{f4e5}", "▣"),
    (LINK, "\u{f44c}", "↗"),
    (SLACK, "\u{f04b1}", "↗"),
    (FILE, "\u{f4fb}", "▤"),
    (EDITED, "\u{f448}", "✎"),
    (EPHEMERAL, "\u{f441}", "◌"),
    (VIRTUAL_WORKSPACE, "\u{f51e}", "≡"),
    (FOLDED, "\u{f44a}", "▸"),
    (UNFOLDED, "\u{f44b}", "▾"),
    (CRUMB, "\u{f460}", "›"),
    (AVATAR, "\u{f0009}", "○"),
    (BROADCAST, "\u{f45f}", "@"),
    (BOT, "\u{f06a9}", "◇"),
    (SENDING, "\u{f43a}", "…"),
    (LIVE, "\u{f0318}", "●"),
    (OFFLINE, "\u{f0319}", "○"),
    (LOADING, "\u{f46a}", "…"),
    (ALERT, "\u{f421}", "!"),
    (SIGNED_IN, "\u{f42e}", "✓"),
];
