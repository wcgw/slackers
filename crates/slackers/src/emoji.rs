//! Which emoji to draw how: standard emoji as their characters, Custom Emoji as pictures once loaded.

use emojis::SkinTone;
use slackers_gfx::Picture;
use slackers_model::{CustomEmoji, CustomEmojiSet, Emoji};

use crate::pictures::{PictureState, Pictures};

/// A Workspace's emoji: its Custom Emoji, by name.
#[derive(Debug, Default)]
pub(crate) struct Emojis {
    custom: CustomEmojiSet,
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

    /// How to draw `emoji` right now, given the `pictures` loaded so far.
    pub(crate) fn drawable<'a>(&'a self, emoji: &'a Emoji, pictures: &'a Pictures) -> Drawable<'a> {
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
            Some(CustomEmoji::Image { url }) => match pictures.state(url) {
                PictureState::Ready(picture) => Drawable::Picture(picture),
                PictureState::Awaiting { requested } => Drawable::Awaiting { url, requested },
                PictureState::Failed => Drawable::Name,
            },
            Some(CustomEmoji::Standard { name }) => {
                standard(name).map_or(Drawable::Name, Drawable::Text)
            }
            None => standard(&emoji.name).map_or(Drawable::Name, Drawable::Text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_emoji_draw_as_text_with_skin_tones() {
        let (emojis, pictures) = (Emojis::default(), Pictures::default());
        assert_eq!(
            emojis.drawable(&Emoji::named("+1"), &pictures),
            Drawable::Text("👍")
        );
        let mut toned = Emoji::named("thumbsup");
        toned.skin_tone = Some(3);
        assert_eq!(emojis.drawable(&toned, &pictures), Drawable::Text("👍🏼"));
        let unknown = Emoji::named("no-such-emoji");
        assert_eq!(emojis.drawable(&unknown, &pictures), Drawable::Name);
    }

    #[test]
    fn slacks_own_characters_win() {
        let emoji = Emoji {
            name: "anything".into(),
            skin_tone: None,
            unicode: Some("🎉".into()),
        };
        let (emojis, pictures) = (Emojis::default(), Pictures::default());
        assert_eq!(emojis.drawable(&emoji, &pictures), Drawable::Text("🎉"));
    }

    #[test]
    fn custom_emoji_follow_their_picture() {
        let mut emojis = Emojis::default();
        let mut custom = CustomEmojiSet::default();
        custom.insert_image("parrot", "https://e/parrot.gif");
        custom.insert_alias("yes", "white_check_mark");
        emojis.set_custom(custom);
        let mut pictures = Pictures::default();

        assert_eq!(
            emojis.drawable(&Emoji::named("yes"), &pictures),
            Drawable::Text("✅")
        );
        let parrot = Emoji::named("parrot");
        let awaiting = |requested| Drawable::Awaiting {
            url: "https://e/parrot.gif",
            requested,
        };
        assert_eq!(emojis.drawable(&parrot, &pictures), awaiting(false));
        pictures.request("https://e/parrot.gif");
        assert_eq!(emojis.drawable(&parrot, &pictures), awaiting(true));
        pictures.failed("https://e/parrot.gif".into());
        assert_eq!(emojis.drawable(&parrot, &pictures), Drawable::Name);
    }
}
