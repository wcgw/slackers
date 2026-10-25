use std::collections::HashMap;

/// An emoji as a Message uses it: by Slack shortcode, such as `thumbsup`, or a Custom Emoji's name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Emoji {
    /// The shortcode, without colons or skin tone.
    pub name: String,
    /// The skin tone asked for, as a Fitzpatrick type from 2 to 6; `None` for the default.
    pub skin_tone: Option<u8>,
    /// The emoji's characters, when Slack sent them along; `None` for Custom Emoji, and when Slack didn't say.
    pub unicode: Option<String>,
}

impl Emoji {
    /// The emoji `name`, with the default skin tone and no characters known.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            skin_tone: None,
            unicode: None,
        }
    }
}

/// A Workspace's Custom Emoji, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomEmojiSet {
    by_name: HashMap<String, Source>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    Image { url: String },
    Alias { of: String },
}

/// What a Custom Emoji name stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomEmoji<'a> {
    /// An image.
    Image {
        /// Where to download it; no credentials needed.
        url: &'a str,
    },
    /// Another name for a standard emoji.
    Standard {
        /// The standard emoji's shortcode, such as `thumbsup`.
        name: &'a str,
    },
}

impl CustomEmojiSet {
    /// Adds the Custom Emoji `name`, drawn from the image at `url`.
    pub fn insert_image(&mut self, name: impl Into<String>, url: impl Into<String>) {
        self.by_name
            .insert(name.into(), Source::Image { url: url.into() });
    }

    /// Adds `name` as another name for the emoji `of`, custom or standard.
    pub fn insert_alias(&mut self, name: impl Into<String>, of: impl Into<String>) {
        self.by_name
            .insert(name.into(), Source::Alias { of: of.into() });
    }

    /// What `name` stands for, following aliases; `None` if this Workspace has no Custom Emoji by that name.
    pub fn resolve(&self, name: &str) -> Option<CustomEmoji<'_>> {
        let mut source = self.by_name.get(name)?;
        // Slack doesn't allow alias cycles, but a bound keeps a bad reply from hanging us.
        for _ in 0..8 {
            match source {
                Source::Image { url } => return Some(CustomEmoji::Image { url }),
                Source::Alias { of } => match self.by_name.get(of) {
                    Some(next) => source = next,
                    None => return Some(CustomEmoji::Standard { name: of }),
                },
            }
        }
        None
    }

    /// Every name it holds, aliases included, in no particular order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    /// How many names it holds, aliases included.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether it holds no Custom Emoji at all.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_images_or_standard_emoji() {
        let mut set = CustomEmojiSet::default();
        set.insert_image("parrot", "https://emoji/parrot.gif");
        set.insert_alias("partyparrot", "parrot");
        set.insert_alias("yes", "thumbsup");

        assert_eq!(
            set.resolve("partyparrot"),
            Some(CustomEmoji::Image {
                url: "https://emoji/parrot.gif"
            })
        );
        assert_eq!(
            set.resolve("yes"),
            Some(CustomEmoji::Standard { name: "thumbsup" })
        );
        assert_eq!(set.resolve("thumbsup"), None);
        let mut names: Vec<&str> = set.names().collect();
        names.sort_unstable();
        assert_eq!(names, ["parrot", "partyparrot", "yes"]);
    }

    #[test]
    fn an_alias_cycle_resolves_to_nothing() {
        let mut set = CustomEmojiSet::default();
        set.insert_alias("a", "b");
        set.insert_alias("b", "a");
        assert_eq!(set.resolve("a"), None);
    }
}
