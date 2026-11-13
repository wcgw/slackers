//! slackers' highlight groups (wcgw-kit's D7): what each part of a Message, the sidebar and Activity is drawn in.
//! Each links to one of the kit's standard groups, so any colorscheme colours it; for tokyonight, slackers brings
//! the colours it always had, from folke/tokyonight.nvim's palettes. Only text and highlights are coloured: the
//! terminal's own background shows everywhere else.

use wcgw_kit::hl::{Group, Theme};

/// Secondary text: times, rules, borders, a Message's gutter.
pub(crate) const DIM: Group = Group::new("SlackersDim");
/// The names of Conversations with nothing unread.
pub(crate) const READ: Group = Group::new("SlackersRead");
/// The names of Conversations with something unread.
pub(crate) const UNREAD: Group = Group::new("SlackersUnread");
/// Icons that want attention without alarm: a Conversation with something unread, a Thread's Replies.
pub(crate) const ACCENT: Group = Group::new("SlackersAccent");
/// Where what's new starts, in a Conversation or a Thread.
pub(crate) const NEW: Group = Group::new("SlackersNew");
/// Someone using Slack right now.
pub(crate) const ACTIVE: Group = Group::new("SlackersActive");
/// A button an app put in a Message.
pub(crate) const BUTTON: Group = Group::new("SlackersButton");
/// The button an app suggests pressing, such as Giphy's Send.
pub(crate) const PRIMARY_BUTTON: Group = Group::new("SlackersPrimaryButton");
/// An @-mention of someone else, and the user's own reactions.
pub(crate) const MENTION: Group = Group::new("SlackersMention");
/// An @-mention of the user, a broadcast reaching them, and Mention badges.
pub(crate) const MENTION_ME: Group = Group::new("SlackersMentionMe");
/// A link in a Message, or a Channel it names.
pub(crate) const LINK: Group = Group::new("SlackersLink");
/// Code in a Message.
pub(crate) const CODE: Group = Group::new("SlackersCode");
/// Something that went wrong, such as a Workspace that couldn't sign in.
pub(crate) const ALERT: Group = Group::new("SlackersAlert");
/// A quote's bar.
pub(crate) const QUOTE: Group = Group::new("SlackersQuote");
/// The names of bots.
pub(crate) const BOT: Group = Group::new("SlackersBot");
/// Section headers, and the name of what a window shows.
pub(crate) const HEADER: Group = Group::new("SlackersHeader");

/// People's names, and the initials of Workspaces without an icon: each always gets the same one.
pub(crate) const AUTHORS: [Group; 8] = [
    Group::new("SlackersAuthor0"),
    Group::new("SlackersAuthor1"),
    Group::new("SlackersAuthor2"),
    Group::new("SlackersAuthor3"),
    Group::new("SlackersAuthor4"),
    Group::new("SlackersAuthor5"),
    Group::new("SlackersAuthor6"),
    Group::new("SlackersAuthor7"),
];

/// slackers' groups but the authors', and the standard ones they are drawn as when a colorscheme says nothing of
/// them.
const GROUPS: [(Group, Group); 17] = [
    (DIM, Group::COMMENT),
    (READ, Group::COMMENT),
    (UNREAD, Group::BOLD),
    (ACCENT, Group::FUNCTION),
    (NEW, Group::DIAGNOSTIC_ERROR),
    (ACTIVE, Group::DIAGNOSTIC_OK),
    (BUTTON, Group::FUNCTION),
    (PRIMARY_BUTTON, Group::DIAGNOSTIC_OK),
    (MENTION, Group::SPECIAL),
    (MENTION_ME, Group::DIAGNOSTIC_WARN),
    (LINK, Group::UNDERLINED),
    (CODE, Group::MARKUP_RAW),
    (ALERT, Group::DIAGNOSTIC_ERROR),
    (QUOTE, Group::MARKUP_QUOTE),
    (BOT, Group::COMMENT),
    (HEADER, Group::BOLD),
    (AUTHORS[0], Group::FUNCTION),
];

/// The authors' groups but the first, and what they link to.
const MORE_AUTHORS: [(Group, Group); 7] = [
    (AUTHORS[1], Group::STRING),
    (AUTHORS[2], Group::DIAGNOSTIC_WARN),
    (AUTHORS[3], Group::IDENTIFIER),
    (AUTHORS[4], Group::KEYWORD),
    (AUTHORS[5], Group::CONSTANT),
    (AUTHORS[6], Group::DIAGNOSTIC_HINT),
    (AUTHORS[7], Group::STATEMENT),
];

/// A tokyonight palette, by its own names: the colours slackers used.
struct Palette {
    fg_dark: &'static str,
    comment: &'static str,
    blue: &'static str,
    blue1: &'static str,
    cyan: &'static str,
    teal: &'static str,
    green: &'static str,
    yellow: &'static str,
    orange: &'static str,
    magenta: &'static str,
    purple: &'static str,
    red: &'static str,
}

/// tokyonight's moon palette, which slackers was drawn in before the kit.
const MOON: Palette = Palette {
    fg_dark: "#828bb8",
    comment: "#636da6",
    blue: "#82aaff",
    blue1: "#65bcff",
    cyan: "#86e1fc",
    teal: "#4fd6be",
    green: "#c3e88d",
    yellow: "#ffc777",
    orange: "#ff966c",
    magenta: "#c099ff",
    purple: "#fca7ea",
    red: "#ff757f",
};

/// tokyonight's night palette, which storm shares.
const NIGHT: Palette = Palette {
    fg_dark: "#a9b1d6",
    comment: "#565f89",
    blue: "#7aa2f7",
    blue1: "#2ac3de",
    cyan: "#7dcfff",
    teal: "#1abc9c",
    green: "#9ece6a",
    yellow: "#e0af68",
    orange: "#ff9e64",
    magenta: "#bb9af7",
    purple: "#9d7cd8",
    red: "#f7768e",
};

/// tokyonight's day palette.
const DAY: Palette = Palette {
    fg_dark: "#6172b0",
    comment: "#848cb5",
    blue: "#2e7de9",
    blue1: "#188092",
    cyan: "#007197",
    teal: "#118c74",
    green: "#587539",
    yellow: "#8c6c3e",
    orange: "#b15c00",
    magenta: "#9854f1",
    purple: "#7847bd",
    red: "#f52a65",
};

impl Palette {
    /// slackers' groups in this palette, as `:hi` lines.
    fn overrides(&self) -> String {
        let authors = [
            self.blue,
            self.green,
            self.yellow,
            self.magenta,
            self.cyan,
            self.orange,
            self.teal,
            self.purple,
        ];
        let mut lines = vec![
            format!("hi SlackersDim guifg={}", self.comment),
            format!("hi SlackersRead guifg={}", self.fg_dark),
            "hi SlackersUnread gui=bold".to_owned(),
            format!("hi SlackersAccent guifg={}", self.blue),
            format!("hi SlackersNew guifg={}", self.red),
            format!("hi SlackersActive guifg={}", self.green),
            format!("hi SlackersButton guifg={}", self.blue),
            format!("hi SlackersPrimaryButton gui=bold guifg={}", self.green),
            format!("hi SlackersMention gui=bold guifg={}", self.blue1),
            format!("hi SlackersMentionMe gui=bold guifg={}", self.yellow),
            format!("hi SlackersLink gui=underline guifg={}", self.teal),
            format!("hi SlackersCode guifg={}", self.green),
            format!("hi SlackersAlert guifg={}", self.red),
            format!("hi SlackersQuote guifg={}", self.comment),
            format!("hi SlackersBot gui=bold guifg={}", self.fg_dark),
            "hi SlackersHeader gui=bold".to_owned(),
        ];
        lines.extend(
            authors
                .iter()
                .enumerate()
                .map(|(index, colour)| format!("hi SlackersAuthor{index} gui=bold guifg={colour}")),
        );
        lines.join("\n")
    }
}

/// The kit's theme with slackers' groups, and their colours in each tokyonight.
#[must_use]
pub(crate) fn theme() -> Theme {
    let mut theme = Theme::new();
    for (group, link) in GROUPS.into_iter().chain(MORE_AUTHORS) {
        theme.define(group, Some(link));
    }
    for (scheme, palette) in [
        ("tokyonight-moon", &MOON),
        ("tokyonight-night", &NIGHT),
        ("tokyonight", &NIGHT),
        ("tokyonight-storm", &NIGHT),
        ("tokyonight-day", &DAY),
    ] {
        theme
            .add_overrides(scheme, palette.overrides())
            .expect("slackers' colours name slackers' groups");
    }
    theme
}

/// The group of `id`'s name, the same every time: an author's, or a Workspace's initial.
pub(crate) fn author(id: &str) -> Group {
    let hash = id.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    AUTHORS[hash % AUTHORS.len()]
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;

    #[test]
    fn tokyonight_moon_draws_slackers_as_it_always_was() {
        let theme = theme();
        assert_eq!(theme.name(), "tokyonight-moon");
        let style = |group| theme.style(group);
        assert_eq!(style(DIM).fg, Some(Color::Rgb(0x63, 0x6d, 0xa6)));
        assert_eq!(style(NEW).fg, Some(Color::Rgb(0xff, 0x75, 0x7f)));
        assert!(style(MENTION_ME).add_modifier.contains(Modifier::BOLD));
        assert_eq!(style(AUTHORS[3]).fg, Some(Color::Rgb(0xc0, 0x99, 0xff)));
    }

    #[test]
    fn other_colorschemes_draw_slackers_groups_as_the_standard_ones_they_link_to() {
        let mut theme = theme();
        theme.colorscheme("default").expect("built in");
        assert_eq!(theme.style(DIM), theme.style(Group::COMMENT));
        assert_eq!(theme.style(LINK), theme.style(Group::UNDERLINED));
        theme.colorscheme("tokyonight-day").expect("built in");
        assert_eq!(theme.style(NEW).fg, Some(Color::Rgb(0xf5, 0x2a, 0x65)));
    }

    #[test]
    fn everyone_keeps_their_colour() {
        assert_eq!(author("U1"), author("U1"));
        assert!(AUTHORS.contains(&author("someone else")));
    }
}
