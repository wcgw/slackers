use std::ops::BitOr;

use crate::{ConversationId, Emoji, UserId};

/// A Message's formatted text, as a sequence of blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RichText {
    /// The blocks, top to bottom; empty for a Message with no text.
    pub blocks: Vec<Block>,
}

/// A paragraph-level piece of [`RichText`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Running text.
    Paragraph(Vec<Inline>),
    /// Quoted text.
    Quote(Vec<Inline>),
    /// Code or other text shown as is, in a fixed-width font. May span several lines.
    Preformatted(String),
    /// A list.
    List {
        /// Whether it's numbered rather than bulleted.
        ordered: bool,
        /// The list items, in order.
        items: Vec<Vec<Inline>>,
    },
}

/// A run of [`RichText`] within a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    /// Text without links or mentions.
    Text {
        /// The characters, which may include line breaks.
        text: String,
        /// How they're styled.
        style: Style,
    },
    /// An emoji, standard or custom.
    Emoji(Emoji),
    /// An @-mention of a person.
    User(UserId),
    /// A link to a Channel.
    Channel(ConversationId),
    /// An `@here`, `@channel` or `@everyone`.
    Broadcast(Broadcast),
    /// An @-mention of a user group, by its id.
    UserGroup(String),
    /// A link.
    Link {
        /// Where it goes.
        url: String,
        /// What to show instead of the URL; `None` to show the URL.
        label: Option<String>,
    },
}

/// Who an `@here`-style mention reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Broadcast {
    /// Members of the Conversation who are active.
    Here,
    /// Every member of the Conversation.
    Channel,
    /// Everyone in the Workspace.
    Everyone,
}

/// How a run of text is styled: any combination of bold, italic, strikethrough and code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style(u8);

impl Style {
    /// Unstyled text.
    pub const PLAIN: Self = Self(0);
    /// Bold.
    pub const BOLD: Self = Self(1);
    /// Italic.
    pub const ITALIC: Self = Self(1 << 1);
    /// Struck through.
    pub const STRIKE: Self = Self(1 << 2);
    /// Inline code.
    pub const CODE: Self = Self(1 << 3);

    /// Whether every style in `other` is set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Style {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl RichText {
    /// Text made of a single unstyled paragraph.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            blocks: vec![Block::Paragraph(vec![Inline::Text {
                text: text.into(),
                style: Style::PLAIN,
            }])],
        }
    }

    /// Every inline run, in reading order.
    pub fn inlines(&self) -> impl Iterator<Item = &Inline> {
        self.blocks
            .iter()
            .flat_map(|block| -> Box<dyn Iterator<Item = &Inline> + '_> {
                match block {
                    Block::Paragraph(inlines) | Block::Quote(inlines) => Box::new(inlines.iter()),
                    Block::Preformatted(_) => Box::new(std::iter::empty()),
                    Block::List { items, .. } => Box::new(items.iter().flatten()),
                }
            })
    }

    /// Whether it @-mentions `user` by name. `@here`-style broadcasts don't count.
    pub fn mentions(&self, user: &UserId) -> bool {
        self.inlines()
            .any(|inline| matches!(inline, Inline::User(id) if id == user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_combine() {
        let style = Style::BOLD | Style::CODE;
        assert!(style.contains(Style::BOLD));
        assert!(style.contains(Style::BOLD | Style::CODE));
        assert!(!style.contains(Style::ITALIC));
        assert!(style.contains(Style::PLAIN));
    }

    #[test]
    fn mentions_look_inside_lists_and_quotes_only_for_people() {
        let me = UserId::new("U1");
        let text = RichText {
            blocks: vec![
                Block::Paragraph(vec![Inline::Broadcast(Broadcast::Channel)]),
                Block::List {
                    ordered: false,
                    items: vec![vec![Inline::User(me.clone())]],
                },
            ],
        };
        assert!(text.mentions(&me));
        assert!(!text.mentions(&UserId::new("U2")));
    }
}
