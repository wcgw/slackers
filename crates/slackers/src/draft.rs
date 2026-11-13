//! Drafts: what the user writes before posting it, whom it @-mentions and what else it refers to, and the word being
//! completed. The kit's composer holds the text being written (its D4); slackers holds what the labels in it stand
//! for.

use slackers_model::{Broadcast, ConversationId, UserId};
use slackers_slack::Part;
use wcgw_kit::shell::Word;

use crate::markdown;

/// What the labels in a Draft stand for: the references picked while writing it, or held by the Message it edits.
///
/// A reference is written as its label, such as `@Ada Lovelace` for an @-mention or `#general` for a Channel, and is
/// posted as the real thing for as long as that label stays whole in the text. Anything else that looks like one,
/// such as an `@ada` typed without picking anyone, is posted as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct References {
    picked: Vec<Picked>,
}

/// A Draft as one would write it: its text, and what its labels stand for. What editing a Message starts with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Draft {
    pub(crate) text: String,
    pub(crate) references: References,
}

/// A word being written that can be completed, by what it starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Completing {
    /// `@` and a name, to @-mention someone or some broadcast: the name, which is empty right after the `@`.
    Mention(String),
    /// `:` and at least two characters of an emoji's name: the name.
    Emoji(String),
    /// `/` and a slash command's name, as the Draft's first word: the name, which is empty right after the `/`.
    Command(String),
}

/// Who an @-mention notifies.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Mentioned {
    Person(UserId),
    Broadcast(Broadcast),
    /// A user group's members, by its id.
    UserGroup(String),
}

/// What a label in a Draft stands for, and posts as.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Reference {
    Mention(Mentioned),
    /// A link to a Channel.
    Channel(ConversationId),
}

/// A reference picked for a Draft.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Picked {
    /// How it's written: `@` and a name for an @-mention, `#` and a name for a Channel.
    label: String,
    reference: Reference,
}

/// What a Draft held when it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Written {
    /// As typed, with picked @-mentions written as their labels.
    pub(crate) text: String,
    references: References,
}

impl Completing {
    /// What `word`, before the cursor, is being completed to, if anything: a word starting with `@` or `:` whose
    /// rest could be a name, or with `/` when it's the Draft's first. A word with either inside, such as an email
    /// address or a time, isn't one.
    pub(crate) fn of(word: &Word<'_>) -> Option<Self> {
        let text = word.text;
        if let Some(name) = text.strip_prefix('/')
            && word.first
        {
            let in_name = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_');
            return name
                .chars()
                .all(in_name)
                .then(|| Self::Command(name.to_owned()));
        }
        if let Some(name) = text.strip_prefix('@') {
            let in_name = |c: char| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '\'');
            return name
                .chars()
                .all(in_name)
                .then(|| Self::Mention(name.to_owned()));
        }
        // Two characters at least, so that `:D` and the like stay as typed.
        let name = text.strip_prefix(':')?;
        let in_name = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '+' | '\'');
        (name.chars().nth(1).is_some() && name.chars().all(in_name))
            .then(|| Self::Emoji(name.to_owned()))
    }
}

impl References {
    /// `label`, or if it stands for something other than `reference` already, the first of `label (2)`,
    /// `label (3)`, … that doesn't.
    pub(crate) fn label_for(&self, label: &str, reference: &Reference) -> String {
        (1..)
            .map(|n| match n {
                1 => label.to_owned(),
                n => format!("{label} ({n})"),
            })
            .find(|label| {
                self.picked
                    .iter()
                    .all(|picked| picked.label != *label || picked.reference == *reference)
            })
            .expect("only as many labels are taken as there are picks")
    }

    /// Notes that `label` stands for `reference`, unless it does already.
    pub(crate) fn hold(&mut self, label: String, reference: Reference) {
        if !self.picked.iter().any(|picked| picked.label == label) {
            self.picked.push(Picked { label, reference });
        }
    }

    /// The labels of the @-mentions `text` holds, in the order they were picked.
    pub(crate) fn mentions(&self, text: &str) -> Vec<&str> {
        let mut held = vec![false; self.picked.len()];
        for part in split(text, &self.picked) {
            if let Split::Picked(index) = part {
                held[index] = true;
            }
        }
        self.picked
            .iter()
            .zip(held)
            .filter(|(picked, held)| *held && matches!(picked.reference, Reference::Mention(_)))
            .map(|(picked, _)| picked.label.as_str())
            .collect()
    }

    /// What sending `text` posts, taking every reference along: none are left for what's written next.
    pub(crate) fn take(&mut self, text: String) -> Written {
        Written {
            text,
            references: std::mem::take(self),
        }
    }

    /// Takes back the references of `written`, which couldn't be sent. A label picked since for someone else keeps
    /// them.
    pub(crate) fn put_back(&mut self, written: Written) {
        for picked in written.references.picked {
            if !self.picked.iter().any(|kept| kept.label == picked.label) {
                self.picked.push(picked);
            }
        }
    }
}

impl Draft {
    /// Writes `text` at the end.
    pub(crate) fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Writes `reference` at the end, as `label`; a label already standing for something else is told apart by a
    /// number.
    pub(crate) fn push_reference(&mut self, label: &str, reference: Reference) {
        let label = self.references.label_for(label, &reference);
        self.text.push_str(&label);
        self.references.hold(label, reference);
    }
}

impl Written {
    /// The slash command it runs, `/` included, and what follows it, when it starts with one: a `/` and a word.
    /// To post text starting with `/`, start it with a space instead, as in Slack.
    pub(crate) fn command(&self) -> Option<(String, Vec<Part>)> {
        let rest = self.text.strip_prefix('/')?;
        let name_length = rest.find(char::is_whitespace).unwrap_or(rest.len());
        if name_length == 0 || rest[..name_length].contains('/') {
            return None;
        }
        let command = format!("/{}", &rest[..name_length]);
        let mut parts = self.parts();
        // The command is all plain text: no label starts with `/`.
        let Some(Part::Text(first)) = parts.first_mut() else {
            return None;
        };
        let after = first.strip_prefix(command.as_str())?;
        *first = after
            .strip_prefix(char::is_whitespace)
            .unwrap_or(after)
            .to_owned();
        if first.is_empty() {
            parts.remove(0);
        }
        Some((command, parts))
    }

    /// What it posts as: its Markdown in Slack's markup, as [`markdown::to_slack`] has it, with what each picked label
    /// stands for in its place.
    pub(crate) fn parts(&self) -> Vec<Part> {
        markdown::to_slack(&self.text)
            .into_iter()
            .flat_map(|part| match part {
                Part::Text(text) => self.referenced(&text),
                other => vec![other],
            })
            .collect()
    }

    /// `text`, a piece of its text, with what each picked label in it stands for in its place.
    fn referenced(&self, text: &str) -> Vec<Part> {
        let picked = &self.references.picked;
        split(text, picked)
            .into_iter()
            .map(|piece| match piece {
                Split::Text(text) => Part::Text(text.to_owned()),
                Split::Picked(index) => match &picked[index].reference {
                    Reference::Mention(Mentioned::Person(id)) => Part::User(id.clone()),
                    Reference::Mention(Mentioned::Broadcast(broadcast)) => {
                        Part::Broadcast(*broadcast)
                    }
                    Reference::Mention(Mentioned::UserGroup(id)) => Part::UserGroup(id.clone()),
                    Reference::Channel(id) => Part::Channel(id.clone()),
                },
            })
            .collect()
    }
}

/// A piece of a Draft's text: plain, or the label of a picked @-mention, by its index among those picked.
enum Split<'a> {
    Text(&'a str),
    Picked(usize),
}

/// `text` in pieces, each label of `picked` found where no letter, digit or `_` runs into it on either side, as in
/// `(@Ada)`, but not in `x@Ada` or `@Adam`. Where several labels could start, the longest wins.
fn split<'a>(text: &'a str, picked: &[Picked]) -> Vec<Split<'a>> {
    let mut by_length: Vec<(usize, &Picked)> = picked.iter().enumerate().collect();
    by_length.sort_by_key(|(_, picked)| std::cmp::Reverse(picked.label.len()));
    let mut parts = Vec::new();
    let (mut plain, mut at) = (0, 0);
    while at < text.len() {
        let starts_word = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let found = by_length.iter().find(|(_, picked)| {
            let rest = &text[at..];
            rest.starts_with(&picked.label)
                && rest[picked.label.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        });
        match found {
            Some((index, picked)) if starts_word => {
                if plain < at {
                    parts.push(Split::Text(&text[plain..at]));
                }
                parts.push(Split::Picked(*index));
                at += picked.label.len();
                plain = at;
            }
            _ => at += text[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    if plain < text.len() {
        parts.push(Split::Text(&text[plain..]));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: &str) -> Reference {
        Reference::Mention(Mentioned::Person(UserId::new(id)))
    }

    fn text(text: &str) -> Part {
        Part::Text(text.into())
    }

    /// What `text`, the word before the cursor, is completed to; the Draft's first word when `first`.
    fn completing(text: &str, first: bool) -> Option<Completing> {
        Completing::of(&Word { text, first })
    }

    fn mention(name: &str) -> Option<Completing> {
        Some(Completing::Mention(name.into()))
    }

    fn emoji(name: &str) -> Option<Completing> {
        Some(Completing::Emoji(name.into()))
    }

    /// `references` picked in order, as completing does: each label in place of the word, and a space.
    fn written(picks: &[(&str, Reference)], rest: &str) -> (String, References) {
        let mut references = References::default();
        let mut text = String::new();
        for (label, reference) in picks {
            let label = references.label_for(label, reference);
            text.push_str(&label);
            text.push(' ');
            references.hold(label, reference.clone());
        }
        text.push_str(rest);
        (text, references)
    }

    #[test]
    fn a_word_starting_with_an_at_is_a_mention_being_written() {
        assert_eq!(completing("@ad", false), mention("ad"));
        assert_eq!(completing("@", true), mention(""));
        assert_eq!(completing("@o'bri", false), mention("o'bri"));
        for not_one in ["hi", "ada@example.com", "@ada,", "(@ada"] {
            assert_eq!(completing(not_one, false), None, "{not_one}");
        }
    }

    #[test]
    fn a_word_starting_with_a_colon_and_two_more_is_an_emoji_being_written() {
        assert_eq!(completing(":ta", false), emoji("ta"));
        assert_eq!(completing(":+1", true), emoji("+1"));
        assert_eq!(completing(":it's-a_ok", false), emoji("it's-a_ok"));
        assert_eq!(completing(":お疲", false), emoji("お疲"));
        for not_one in [":", ":D", ":-)", "12:30", "note:ab", ":tada:", "::std"] {
            assert_eq!(completing(not_one, false), None, "{not_one}");
        }
    }

    #[test]
    fn a_slash_and_a_word_starting_a_draft_is_a_command() {
        let command = |name: &str| Some(Completing::Command(name.into()));
        assert_eq!(completing("/", true), command(""));
        assert_eq!(completing("/gi", true), command("gi"));
        assert_eq!(completing("/my-app_x", true), command("my-app_x"));
        assert_eq!(completing("/gi", false), None, "not the first word");
        for not_one in ["//x", "/a.b"] {
            assert_eq!(completing(not_one, true), None, "{not_one}");
        }

        let (mention, mut references) = written(&[("@Ada", person("U1"))], "");
        assert_eq!(
            references
                .take(format!("/giphy cats and {mention}"))
                .command(),
            Some((
                "/giphy".to_owned(),
                vec![text("cats and "), Part::User(UserId::new("U1")), text(" ")]
            ))
        );
        let plain = |typed: &str| References::default().take(typed.to_owned());
        assert_eq!(
            plain("/dnd").command(),
            Some(("/dnd".to_owned(), Vec::new()))
        );
        for typed in [" /giphy cats", "/ x", "//x", "a/b"] {
            assert_eq!(plain(typed).command(), None, "{typed}");
        }
    }

    #[test]
    fn sending_turns_a_picked_label_into_the_mention() {
        let (typed, mut references) = written(&[("@Ada Lovelace", person("U1"))], "!");
        let typed = format!("thanks {typed}");
        assert_eq!(references.mentions(&typed), ["@Ada Lovelace"]);
        assert_eq!(
            references.take(typed).parts(),
            [text("thanks "), Part::User(UserId::new("U1")), text(" !")]
        );
        assert!(references.mentions("@Ada Lovelace").is_empty(), "all taken");
    }

    #[test]
    fn only_whole_labels_starting_a_word_are_mentions() {
        let (typed, mut references) = written(&[("@Ada", person("U1"))], "");
        let typed = typed + "x@Ada @Adam @Ada_ @Ada, @Ada";
        assert_eq!(
            references.take(typed).parts(),
            [
                Part::User(UserId::new("U1")),
                text(" x@Ada @Adam @Ada_ "),
                Part::User(UserId::new("U1")),
                text(", "),
                Part::User(UserId::new("U1")),
            ]
        );
    }

    #[test]
    fn the_longest_label_wins_and_namesakes_are_numbered() {
        let (typed, mut references) = written(
            &[
                ("@Ada", person("U1")),
                ("@Ada Lovelace", person("U2")),
                ("@Ada", person("U3")),
                ("@Ada", person("U1")),
            ],
            "",
        );
        assert_eq!(typed, "@Ada @Ada Lovelace @Ada (2) @Ada ");
        let mentioned: Vec<Part> = references
            .take(typed)
            .parts()
            .into_iter()
            .filter(|part| *part != text(" "))
            .collect();
        assert_eq!(
            mentioned,
            ["U1", "U2", "U3", "U1"].map(|id| Part::User(UserId::new(id)))
        );
    }

    #[test]
    fn edited_text_keeps_the_labels_left_whole() {
        let here = Reference::Mention(Mentioned::Broadcast(Broadcast::Here));
        let (_, mut references) = written(&[("@Ada", person("U1")), ("@here", here)], "");
        let typed = "@Ada, @her".to_owned();
        assert_eq!(references.mentions(&typed), ["@Ada"]);
        assert_eq!(
            references.take(typed).parts(),
            [Part::User(UserId::new("U1")), text(", @her")]
        );
    }

    #[test]
    fn channels_and_groups_post_as_what_their_labels_stand_for() {
        let mut draft = Draft::default();
        draft.push_text("see ");
        draft.push_reference("#general", Reference::Channel(ConversationId::new("C1")));
        draft.push_text(", ");
        let group = Reference::Mention(Mentioned::UserGroup("S1".into()));
        draft.push_reference("@group", group);
        draft.push_text(" and ");
        draft.push_reference("#general", Reference::Channel(ConversationId::new("C2")));
        assert_eq!(
            draft.text, "see #general, @group and #general (2)",
            "another Channel by the same name is told apart"
        );
        assert_eq!(
            draft.references.mentions(&draft.text),
            ["@group"],
            "only those notified"
        );
        assert_eq!(
            draft.references.take(draft.text.clone()).parts(),
            [
                text("see "),
                Part::Channel(ConversationId::new("C1")),
                text(", "),
                Part::UserGroup("S1".into()),
                text(" and "),
                Part::Channel(ConversationId::new("C2")),
            ]
        );
    }

    #[test]
    fn its_markdown_posts_as_slacks_markup_around_the_labels() {
        let (typed, mut references) = written(&[("@Ada", person("U1"))], "");
        let typed = format!("# For {typed}\n**see [the docs](https://a.com), @Ada**");
        assert_eq!(
            references.take(typed).parts(),
            [
                text("*For "),
                Part::User(UserId::new("U1")),
                text("*\n*see "),
                Part::Link {
                    url: "https://a.com".into(),
                    label: "the docs".into()
                },
                text(", "),
                Part::User(UserId::new("U1")),
                text("*"),
            ]
        );
    }

    #[test]
    fn what_wasnt_sent_keeps_its_references_unless_a_label_was_taken_since() {
        let (typed, mut references) = written(&[("@Ada", person("U1"))], "");
        let sent = references.take(typed);
        let (_, mut since) = written(&[("@Bob", person("U2"))], "");
        since.put_back(sent);
        assert_eq!(since.mentions("@Ada @Bob"), ["@Bob", "@Ada"]);
    }
}
