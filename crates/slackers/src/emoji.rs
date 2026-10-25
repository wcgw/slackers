//! Which emoji to draw how: standard emoji as their characters, Custom Emoji as pictures once loaded. And which emoji
//! to offer for a name being written.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::LazyLock;

use emojis::SkinTone;
use slackers_gfx::Picture;
use slackers_model::{CustomEmoji, CustomEmojiSet, Emoji};

use crate::pictures::{PictureState, Pictures};
use crate::search;

/// A Workspace's emoji: its Custom Emoji, by name, the standard emoji Slack lists, and how much the user uses each.
#[derive(Debug, Default)]
pub(crate) struct Emojis {
    custom: CustomEmojiSet,
    /// Every emoji to offer: the standard ones in Slack's order, then the Custom Emoji by name.
    offered: Vec<Offered>,
    /// How many times the user used each emoji, by name.
    used: HashMap<String, u32>,
    /// The last completion: the name and limit it was for, and what it found. Matching every name a large
    /// Workspace has takes milliseconds, and the app asks again after every event.
    completed: Option<(String, usize, Vec<EmojiMatch>)>,
}

/// An emoji to offer.
#[derive(Debug)]
struct Offered {
    /// The name Slack lists it under.
    name: String,
    /// Other names it goes by.
    aliases: Vec<&'static str>,
}

/// An emoji to offer for a name being written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmojiMatch {
    /// What to write it as, between colons.
    pub(crate) name: String,
    /// Another name it goes by, which matched where `name` didn't, or better; empty when `name` matched.
    pub(crate) alias: &'static str,
    /// Which characters of `name` matched, by char index, ascending; empty when `alias` matched.
    pub(crate) positions: Vec<usize>,
}

/// How well an emoji matches a name being written, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    /// Whether the name written falls short of all of one of its names.
    partial: bool,
    /// How many times the user used it.
    used: Reverse<u32>,
    /// Whether the name written starts only a later word of its names, rather than one of them.
    later_word: bool,
    /// Where it's offered.
    order: usize,
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
    /// Sets the Workspace's Custom Emoji, and the `standard` emoji, by the names Slack lists them under, most common
    /// first.
    pub(crate) fn set(&mut self, custom: CustomEmojiSet, standard: Vec<String>) {
        let mut custom_names: Vec<&str> = custom.names().collect();
        custom_names.sort_unstable();
        let standard = standard
            .into_iter()
            // A Workspace's Custom Emoji shadow standard ones of the same name, as in Slack.
            .filter(|name| custom.resolve(name).is_none())
            .map(|name| Offered {
                aliases: standard_emoji(&name)
                    .into_iter()
                    .flat_map(emojis::Emoji::shortcodes)
                    .filter(|alias| *alias != name)
                    .collect(),
                name,
            });
        let custom_names = custom_names.into_iter().map(|name| Offered {
            name: name.to_owned(),
            aliases: Vec::new(),
        });
        self.offered = standard.chain(custom_names).collect();
        self.custom = custom;
        self.completed = None;
    }

    /// Sets how many times the user used each emoji, by name.
    pub(crate) fn set_used(&mut self, used: HashMap<String, u32>) {
        self.used = used;
        self.completed = None;
    }

    /// Counts one more use of the emoji `name`.
    pub(crate) fn note_use(&mut self, name: &str) {
        *self.used.entry(name.to_owned()).or_default() += 1;
        self.completed = None;
    }

    /// The emoji to offer for `name`, being written, best first, at most `limit` of them: those with a name `name`
    /// starts a word of, ignoring case. All of a name comes first, then the most used, then those `name` starts a
    /// name of rather than a later word, then standard emoji, most common first, then Custom Emoji by name.
    pub(crate) fn complete(&mut self, name: &str, limit: usize) -> Vec<EmojiMatch> {
        if let Some((last, last_limit, found)) = &self.completed
            && last == name
            && *last_limit == limit
        {
            return found.clone();
        }
        let mut ranked: Vec<(Rank, &Offered, usize, Vec<usize>)> = self
            .offered
            .iter()
            .enumerate()
            .filter_map(|(order, offered)| {
                // Its own name wins a tie.
                let names =
                    std::iter::once(offered.name.as_str()).chain(offered.aliases.iter().copied());
                let ((partial, later_word), which, positions) = names
                    .enumerate()
                    .filter_map(|(which, candidate)| {
                        let positions = search::word_start(candidate, name)?;
                        let partial = positions.len() < candidate.chars().count();
                        let later_word = positions.first().is_some_and(|&at| at > 0);
                        Some(((partial, later_word), which, positions))
                    })
                    .min_by_key(|(tier, which, _)| (*tier, *which))?;
                let used = self.used.get(&offered.name).copied().unwrap_or(0);
                let rank = Rank {
                    partial,
                    used: Reverse(used),
                    later_word,
                    order,
                };
                Some((rank, offered, which, positions))
            })
            .collect();
        ranked.sort_unstable_by_key(|(rank, ..)| *rank);
        let found: Vec<EmojiMatch> = ranked
            .into_iter()
            .take(limit)
            .map(|(_, offered, which, positions)| match which {
                0 => EmojiMatch {
                    name: offered.name.clone(),
                    alias: "",
                    positions,
                },
                alias => EmojiMatch {
                    name: offered.name.clone(),
                    alias: offered.aliases[alias - 1],
                    positions: Vec::new(),
                },
            })
            .collect();
        self.completed = Some((name.to_owned(), limit, found.clone()));
        found
    }

    /// How to draw `emoji` right now, given the `pictures` loaded so far.
    pub(crate) fn drawable<'a>(&'a self, emoji: &'a Emoji, pictures: &'a Pictures) -> Drawable<'a> {
        if let Some(text) = emoji.unicode.as_deref() {
            return Drawable::Text(text);
        }
        let standard = |name: &str| {
            let base = standard_emoji(name)?;
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

/// The standard emoji Slack calls `name`, if it's one `emojis` knows. Slack's names are mostly GitHub's shortcodes,
/// which `emojis` goes by; most of the others are a flag's region code, or the emoji's Unicode name.
fn standard_emoji(name: &str) -> Option<&'static emojis::Emoji> {
    emojis::get_by_shortcode(name)
        .or_else(|| emojis::get_by_shortcode(&name.replace('-', "_")))
        .or_else(|| flag(name))
        .or_else(|| BY_UNICODE_NAME.get(&words(name)).copied())
}

/// Standard emoji by their Unicode name, as [`words`] writes it.
static BY_UNICODE_NAME: LazyLock<HashMap<String, &'static emojis::Emoji>> = LazyLock::new(|| {
    let mut by_name = HashMap::new();
    for emoji in emojis::iter() {
        by_name.entry(words(emoji.name())).or_insert(emoji);
    }
    by_name
});

/// The flag Slack calls `name`, as in `flag-fr`: that of the region with that two-letter code.
fn flag(name: &str) -> Option<&'static emojis::Emoji> {
    let code = name.strip_prefix("flag-")?;
    // Other characters would turn into other emoji, or none at all.
    if !code.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return None;
    }
    // Each letter as its regional indicator symbol, 🇦 to 🇿. Only two make a flag.
    let letters: Option<String> = code
        .chars()
        .map(|letter| char::from_u32(0x1F1E6 + u32::from(letter) - u32::from('a')))
        .collect();
    emojis::get(&letters?)
}

/// `name`'s words in lower case, a space apart: `admission_tickets` and "admission tickets" are both
/// "admission tickets".
fn words(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emojis(custom: &[&str], standard: &[&str]) -> Emojis {
        let mut set = CustomEmojiSet::default();
        for name in custom {
            set.insert_image(*name, format!("https://e/{name}.gif"));
        }
        let mut emojis = Emojis::default();
        emojis.set(set, standard.iter().map(|name| name.to_string()).collect());
        emojis
    }

    fn names(found: &[EmojiMatch]) -> Vec<&str> {
        found.iter().map(|found| found.name.as_str()).collect()
    }

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
    fn slacks_names_that_github_lacks_are_found_by_region_or_unicode_name() {
        let char_of = |name| standard_emoji(name).map(emojis::Emoji::as_str);
        assert_eq!(char_of("male-detective"), Some("🕵️\u{200d}♂️"));
        assert_eq!(char_of("flag-fr"), Some("🇫🇷"));
        assert_eq!(char_of("admission_tickets"), Some("🎟️"));
        // Not even `flag-ѻ`, which would come out as 😀 were it read as a region.
        for not_one in [
            "flag-",
            "flag-f",
            "flag-FR",
            "flag-fra",
            "flag-ѻ",
            "no_such_thing",
        ] {
            assert_eq!(char_of(not_one), None, "{not_one}");
        }
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
        emojis.set(custom, Vec::new());
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
        pictures.request("https://e/parrot.gif", slackers_gfx::Fit::Emoji);
        assert_eq!(emojis.drawable(&parrot, &pictures), awaiting(true));
        pictures.failed("https://e/parrot.gif".into());
        assert_eq!(emojis.drawable(&parrot, &pictures), Drawable::Name);
    }

    #[test]
    fn completing_offers_whole_names_then_the_most_used_then_name_starts_then_slacks_order() {
        let mut emojis = emojis(
            &["partyparrot", "parrot", "fast_parrot", "par"],
            &["partying_face", "parking", "part_alternation_mark"],
        );
        assert_eq!(
            names(&emojis.complete("par", 10)),
            [
                "par",
                "partying_face",
                "parking",
                "part_alternation_mark",
                "parrot",
                "partyparrot",
                "fast_parrot"
            ]
        );
        emojis.set_used(HashMap::from([("fast_parrot".to_owned(), 2)]));
        emojis.note_use("parking");
        emojis.note_use("parking");
        emojis.note_use("parking");
        assert_eq!(
            names(&emojis.complete("Par", 4)),
            ["par", "parking", "fast_parrot", "partying_face"]
        );
        let found = emojis.complete("parr", 2);
        assert_eq!(names(&found), ["fast_parrot", "parrot"]);
        assert_eq!(found[0].positions, [5, 6, 7, 8]);
        assert_eq!(found[1].positions, [0, 1, 2, 3]);
        assert!(emojis.complete("parx", 10).is_empty());
    }

    #[test]
    fn completing_again_follows_what_changed_since() {
        let mut emojis = emojis(&["parrot"], &["parking"]);
        assert_eq!(names(&emojis.complete("par", 1)), ["parking"]);
        assert_eq!(names(&emojis.complete("par", 2)), ["parking", "parrot"]);
        emojis.set_used(HashMap::from([("parrot".to_owned(), 1)]));
        assert_eq!(names(&emojis.complete("par", 2)), ["parrot", "parking"]);
        let mut custom = CustomEmojiSet::default();
        custom.insert_image("party", "https://e/party.gif");
        emojis.set(custom, Vec::new());
        assert_eq!(names(&emojis.complete("par", 2)), ["party"]);
    }

    #[test]
    fn a_standard_emoji_is_found_by_its_other_names_and_offered_by_slacks() {
        let mut emojis = emojis(&["thumbsup_all"], &["+1", "thinking_face"]);
        let found = emojis.complete("thumbsup", 10);
        assert_eq!(names(&found), ["+1", "thumbsup_all"]);
        assert_eq!(
            (found[0].alias, found[0].positions.as_slice()),
            ("thumbsup", &[][..])
        );
        assert_eq!(found[1].alias, "");
        let found = emojis.complete("thinking", 10);
        assert_eq!(names(&found), ["thinking_face"]);
        assert_eq!(
            found[0].alias, "thinking",
            "a whole other name beats part of its own"
        );
        assert_eq!(
            emojis.complete("think", 10)[0].alias,
            "",
            "its own name wins a tie"
        );
    }

    #[test]
    fn a_custom_emoji_shadows_a_standard_one_of_the_same_name() {
        let mut emojis = emojis(&["tada"], &["tada", "taco"]);
        assert_eq!(names(&emojis.complete("ta", 10)), ["taco", "tada"]);
    }
}
