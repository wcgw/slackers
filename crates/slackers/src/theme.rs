//! slackers' colours: Tokyo Night Moon, from folke/tokyonight.nvim's `lua/tokyonight/colors/moon.lua`, matching the
//! terminal theme of the same name. Only text and highlights are coloured: everywhere else the terminal's own
//! background shows, with its opacity and blur.

use ratatui::style::{Color, Modifier, Style};

/// Tokyo Night Moon's palette, by its own names.
mod palette {
    use ratatui::style::Color;

    pub(super) const FG_DARK: Color = Color::Rgb(0x82, 0x8b, 0xb8);
    pub(super) const COMMENT: Color = Color::Rgb(0x63, 0x6d, 0xa6);
    pub(super) const BG_HIGHLIGHT: Color = Color::Rgb(0x2f, 0x33, 0x4d);
    /// `blue0` blended 40% over `bg`, as tokyonight computes `bg_visual`.
    pub(super) const BG_VISUAL: Color = Color::Rgb(0x2d, 0x3f, 0x76);
    pub(super) const BLUE: Color = Color::Rgb(0x82, 0xaa, 0xff);
    pub(super) const BLUE1: Color = Color::Rgb(0x65, 0xbc, 0xff);
    pub(super) const CYAN: Color = Color::Rgb(0x86, 0xe1, 0xfc);
    pub(super) const TEAL: Color = Color::Rgb(0x4f, 0xd6, 0xbe);
    pub(super) const GREEN: Color = Color::Rgb(0xc3, 0xe8, 0x8d);
    pub(super) const YELLOW: Color = Color::Rgb(0xff, 0xc7, 0x77);
    pub(super) const ORANGE: Color = Color::Rgb(0xff, 0x96, 0x6c);
    pub(super) const MAGENTA: Color = Color::Rgb(0xc0, 0x99, 0xff);
    pub(super) const PURPLE: Color = Color::Rgb(0xfc, 0xa7, 0xea);
    pub(super) const RED: Color = Color::Rgb(0xff, 0x75, 0x7f);
}

/// Secondary text: times, rules, hints, borders.
pub(crate) const DIM: Style = Style::new().fg(palette::COMMENT);
/// The names of Conversations with nothing unread.
pub(crate) const READ: Style = Style::new().fg(palette::FG_DARK);
/// What has focus, or wants attention without alarm.
pub(crate) const ACCENT: Style = Style::new().fg(palette::BLUE);
/// A button an app put in a Message.
pub(crate) const BUTTON: Style = Style::new().fg(palette::BLUE);
/// The button an app suggests pressing, such as Giphy's Send.
pub(crate) const PRIMARY_BUTTON: Style =
    Style::new().fg(palette::GREEN).add_modifier(Modifier::BOLD);
/// An @-mention of someone else.
pub(crate) const MENTION: Style = Style::new().fg(palette::BLUE1).add_modifier(Modifier::BOLD);
/// An @-mention of the user, or a broadcast reaching them.
pub(crate) const MENTION_ME: Style = Style::new()
    .fg(palette::YELLOW)
    .add_modifier(Modifier::BOLD);
pub(crate) const LINK: Style = Style::new()
    .fg(palette::TEAL)
    .add_modifier(Modifier::UNDERLINED);
pub(crate) const CODE: Style = Style::new().fg(palette::GREEN);
pub(crate) const ALERT: Style = Style::new().fg(palette::RED);
/// The characters of a name that matched a search.
pub(crate) const MATCH: Style = Style::new()
    .fg(palette::ORANGE)
    .add_modifier(Modifier::BOLD);
/// The live connection, while it's up.
pub(crate) const LIVE: Style = Style::new().fg(palette::GREEN);
/// The live connection, while it's coming back.
pub(crate) const RECONNECTING: Style = Style::new().fg(palette::YELLOW);
/// The Insert mode indicator.
pub(crate) const INSERT: Style = Style::new().fg(palette::GREEN).add_modifier(Modifier::BOLD);
/// The selection, in the pane with focus.
pub(crate) const SELECTED: Style = Style::new().bg(palette::BG_VISUAL);
/// The selection in a pane without focus, and the open Conversation in the sidebar.
pub(crate) const SELECTED_UNFOCUSED: Style = Style::new().bg(palette::BG_HIGHLIGHT);
/// Bots' names.
pub(crate) const BOT: Color = palette::FG_DARK;
/// People's names: each person always gets the same one.
pub(crate) const AUTHORS: [Color; 8] = [
    palette::BLUE,
    palette::GREEN,
    palette::YELLOW,
    palette::MAGENTA,
    palette::CYAN,
    palette::ORANGE,
    palette::TEAL,
    palette::PURPLE,
];
