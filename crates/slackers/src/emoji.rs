//! Which emoji to draw how: standard emoji as their characters, Custom Emoji as pictures once loaded.

use std::collections::HashMap;

use emojis::SkinTone;
use slackers_gfx::Picture;
use slackers_model::{CustomEmoji, CustomEmojiSet, Emoji};

/// A Workspace's emoji: its Custom Emoji and the pictures loaded for them, by image URL.
#[derive(Debug, Default)]
pub(crate) struct Emojis {
    custom: CustomEmojiSet,
    pictures: HashMap<String, Loading>,
}

#[derive(Debug)]
enum Loading {
    Pending,
    Ready(Picture),
    Failed,
}

/// How to draw one emoji.
#[derive(Debug, PartialEq)]
pub(crate) enum Drawable<'a> {
    /// As these characters.
    Text(&'a str),
    /// As this picture.
    Picture(&'a Picture),
    /// As its `:name:`, for now: the picture at `url` isn't loaded yet, and hasn't been asked for unless
    /// `requested`.
    Awaiting { url: &'a str, requested: bool },
    /// As its `:name:`: there's nothing better.
    Name,
}

impl Emojis {
    pub(crate) fn set_custom(&mut self, custom: CustomEmojiSet) {
        self.custom = custom;
    }

    /// How to draw `emoji` right now.
    pub(crate) fn drawable<'a>(&'a self, emoji: &'a Emoji) -> Drawable<'a> {
        if let Some(text) = emoji.unicode.as_deref() {
            return Drawable::Text(text);
        }
        let standard = |name: &str| {
            let base = emojis::get_by_shortcode(name)?;
            let tone = match emoji.skin_tone {
                Some(2) => SkinTone::Light,
                Some(3) => SkinTone::MediumLight,
                Some(4) => SkinTone::Medium,
                Some(5) => SkinTone::MediumDark,
                Some(6) => SkinTone::Dark,
                _ => SkinTone::Default,
            };
            Some(base.with_skin_tone(tone).unwrap_or(base).as_str())
        };
        // A Workspace's Custom Emoji shadow standard ones of the same name, as in Slack.
        match self.custom.resolve(&emoji.name) {
            Some(CustomEmoji::Image { url }) => match self.pictures.get(url) {
                Some(Loading::Ready(picture)) => Drawable::Picture(picture),
                Some(Loading::Pending) => Drawable::Awaiting {
                    url,
                    requested: true,
                },
                Some(Loading::Failed) => Drawable::Name,
                None => Drawable::Awaiting {
                    url,
                    requested: false,
                },
            },
            Some(CustomEmoji::Standard { name }) => {
                standard(name).map_or(Drawable::Name, Drawable::Text)
            }
            None => standard(&emoji.name).map_or(Drawable::Name, Drawable::Text),
        }
    }

    /// Notes that the picture at `url` is being fetched. Returns whether it wasn't already: whether to fetch it.
    pub(crate) fn request(&mut self, url: &str) -> bool {
        if self.pictures.contains_key(url) {
            return false;
        }
        self.pictures.insert(url.to_owned(), Loading::Pending);
        true
    }

    /// Whether `url` is a picture these emoji are waiting for.
    pub(crate) fn awaits(&self, url: &str) -> bool {
        matches!(self.pictures.get(url), Some(Loading::Pending))
    }

    pub(crate) fn loaded(&mut self, url: String, picture: Picture) {
        self.pictures.insert(url, Loading::Ready(picture));
    }

    /// Notes that the picture at `url` couldn't be loaded, so that it's drawn by name and not asked for again.
    pub(crate) fn failed(&mut self, url: String) {
        self.pictures.insert(url, Loading::Failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_emoji_draw_as_text_with_skin_tones() {
        let emojis = Emojis::default();
        assert_eq!(emojis.drawable(&Emoji::named("+1")), Drawable::Text("👍"));
        let mut toned = Emoji::named("thumbsup");
        toned.skin_tone = Some(3);
        assert_eq!(emojis.drawable(&toned), Drawable::Text("👍🏼"));
        assert_eq!(
            emojis.drawable(&Emoji::named("no-such-emoji")),
            Drawable::Name
        );
    }

    #[test]
    fn slacks_own_characters_win() {
        let emoji = Emoji {
            name: "anything".into(),
            skin_tone: None,
            unicode: Some("🎉".into()),
        };
        assert_eq!(Emojis::default().drawable(&emoji), Drawable::Text("🎉"));
    }

    #[test]
    fn custom_emoji_await_their_picture_once() {
        let mut emojis = Emojis::default();
        let mut custom = CustomEmojiSet::default();
        custom.insert_image("parrot", "https://e/parrot.gif");
        custom.insert_alias("yes", "white_check_mark");
        emojis.set_custom(custom);

        assert_eq!(emojis.drawable(&Emoji::named("yes")), Drawable::Text("✅"));
        let parrot = Emoji::named("parrot");
        assert_eq!(
            emojis.drawable(&parrot),
            Drawable::Awaiting {
                url: "https://e/parrot.gif",
                requested: false
            }
        );
        assert!(emojis.request("https://e/parrot.gif"));
        assert!(
            !emojis.request("https://e/parrot.gif"),
            "asked for only once"
        );
        assert!(emojis.awaits("https://e/parrot.gif"));
        assert_eq!(
            emojis.drawable(&parrot),
            Drawable::Awaiting {
                url: "https://e/parrot.gif",
                requested: true
            }
        );
        emojis.failed("https://e/parrot.gif".into());
        assert_eq!(emojis.drawable(&parrot), Drawable::Name);
    }
}
