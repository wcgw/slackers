//! Drafts: what the user writes before posting it, whom it @-mentions and what else it refers to, and the word being
//! completed.

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;
use ratatui_textarea::{CursorMove, TextArea};
use slackers_model::{Broadcast, ConversationId, UserId};
use slackers_slack::Part;

use crate::app::Place;

/// The text written for one Place, and the references picked while writing it, or held by the Message it edits.
///
/// A reference is written as its label, such as `@Ada Lovelace` for an @-mention or `#general` for a Channel, and is
/// posted as the real thing for as long as that label stays whole in the text. Anything else that looks like one,
/// such as an `@ada` typed without picking anyone, is posted as typed.
#[derive(Debug)]
pub(crate) struct Draft {
    area: TextArea<'static>,
    picked: Vec<Picked>,
    /// What an empty Draft shows: whether it's a Message or a Reply.
    placeholder: &'static str,
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
    /// A link to this URL, shown as the label.
    Link(String),
}

/// A reference picked for a Draft.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Picked {
    /// How it's written: `@` and a name for an @-mention, `#` and a name for a Channel, a link's label.
    label: String,
    reference: Reference,
}

/// What a Draft held when it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Written {
    /// As typed, with picked @-mentions written as their labels.
    pub(crate) text: String,
    picked: Vec<Picked>,
}

impl Draft {
    /// An empty Draft for `place`.
    pub(crate) fn new(place: &Place) -> Self {
        let placeholder = if place.thread.is_some() {
            "reply"
        } else {
            "message"
        };
        Self {
            area: text_area("", placeholder),
            picked: Vec::new(),
            placeholder,
        }
    }

    pub(crate) fn lines(&self) -> &[String] {
        self.area.lines()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.area.is_empty()
    }

    /// Its text, lines joined by `\n`.
    pub(crate) fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    /// Edits it as `key` says, as a text field does.
    pub(crate) fn input(&mut self, key: KeyEvent) {
        self.area.input(key);
    }

    pub(crate) fn insert_newline(&mut self) {
        self.area.insert_newline();
    }

    /// Replaces its text with `text`, say as an external editor left it, with the cursor at the end. @-mentions
    /// picked before stay picked wherever their labels are still whole.
    pub(crate) fn set_text(&mut self, text: &str) {
        self.area = text_area(text, self.placeholder);
    }

    /// The word being completed, if one is: the word before the cursor, when it starts with `@` or `:` and the
    /// rest could be a name. A word with either inside, such as an email address or a time, isn't one.
    pub(crate) fn completing(&self) -> Option<Completing> {
        let word = self.word()?;
        if let Some(name) = word.strip_prefix('/')
            && self.area.cursor().0 == 0
            && self.lines()[0].starts_with(word)
        {
            let in_name = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_');
            return name
                .chars()
                .all(in_name)
                .then(|| Completing::Command(name.to_owned()));
        }
        if let Some(name) = word.strip_prefix('@') {
            let in_name = |c: char| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '\'');
            return name
                .chars()
                .all(in_name)
                .then(|| Completing::Mention(name.to_owned()));
        }
        // Two characters at least, so that `:D` and the like stay as typed.
        let name = word.strip_prefix(':')?;
        let in_name = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '+' | '\'');
        (name.chars().nth(1).is_some() && name.chars().all(in_name))
            .then(|| Completing::Emoji(name.to_owned()))
    }

    /// The word before the cursor: what's between the cursor and the whitespace before it, or the line's start.
    fn word(&self) -> Option<&str> {
        let cursor = self.area.cursor();
        let line = self.area.lines().get(cursor.0)?;
        let end = line
            .char_indices()
            .nth(cursor.1)
            .map_or(line.len(), |(at, _)| at);
        let before = &line[..end];
        Some(
            before
                .rsplit_once(char::is_whitespace)
                .map_or(before, |(_, word)| word),
        )
    }

    /// Writes `with` and a space in place of the word being completed, if there's one.
    fn complete(&mut self, with: &str) -> bool {
        if self.completing().is_none() {
            return false;
        }
        // The word, its `@` or `:` included.
        let written = self.word().map_or(0, |word| word.chars().count());
        for _ in 0..written {
            self.area.delete_char();
        }
        self.area.insert_str(format!("{with} "));
        true
    }

    /// Writes an @-mention of `mentioned`, called `name`, and a space, in place of the word being completed. A name
    /// already picked for someone else is told apart by a number.
    pub(crate) fn pick(&mut self, mentioned: Mentioned, name: &str) {
        let reference = Reference::Mention(mentioned);
        let label = self.label_for(&format!("@{name}"), &reference);
        if self.complete(&label) {
            self.hold(label, reference);
        }
    }

    /// Writes `text` at the end.
    pub(crate) fn push_text(&mut self, text: &str) {
        self.area.move_cursor(CursorMove::Bottom);
        self.area.move_cursor(CursorMove::End);
        self.area.insert_str(text);
    }

    /// Writes `reference` at the end, as `label`; a label already standing for something else is told apart by a
    /// number.
    pub(crate) fn push_reference(&mut self, label: &str, reference: Reference) {
        let label = self.label_for(label, &reference);
        self.push_text(&label);
        self.hold(label, reference);
    }

    /// `label`, or if it stands for something other than `reference` already, the first of `label (2)`,
    /// `label (3)`, … that doesn't.
    fn label_for(&self, label: &str, reference: &Reference) -> String {
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
    fn hold(&mut self, label: String, reference: Reference) {
        if !self.picked.iter().any(|picked| picked.label == label) {
            self.picked.push(Picked { label, reference });
        }
    }

    /// Writes the slash command `name`, `/` included, and a space, in place of the word being completed.
    pub(crate) fn pick_command(&mut self, name: &str) {
        self.complete(name);
    }

    /// Writes the emoji `name`, between colons, and a space, in place of the word being completed. Slack draws it
    /// once posted.
    pub(crate) fn pick_emoji(&mut self, name: &str) {
        self.complete(&format!(":{name}:"));
    }

    /// The labels of the @-mentions its text holds, in the order they were picked.
    pub(crate) fn mentions(&self) -> impl Iterator<Item = &str> {
        let text = self.text();
        let mut held = vec![false; self.picked.len()];
        for part in split(&text, &self.picked) {
            if let Split::Picked(index) = part {
                held[index] = true;
            }
        }
        self.picked
            .iter()
            .zip(held)
            .filter(|(picked, held)| *held && matches!(picked.reference, Reference::Mention(_)))
            .map(|(picked, _)| picked.label.as_str())
    }

    /// Takes what it holds, leaving it empty; or `None`, leaving it as it is, when it holds only blanks.
    pub(crate) fn take(&mut self) -> Option<Written> {
        let text = self.text();
        if text.trim().is_empty() {
            return None;
        }
        self.area = text_area("", self.placeholder);
        Some(Written {
            text,
            picked: std::mem::take(&mut self.picked),
        })
    }

    /// Puts back `written`, which couldn't be sent, ahead of anything written since.
    pub(crate) fn put_back(&mut self, written: Written) {
        let since = self.text();
        let text = if since.is_empty() {
            written.text
        } else {
            format!("{}\n{since}", written.text)
        };
        self.set_text(&text);
        // A label picked since for someone else keeps them.
        for picked in written.picked {
            if !self.picked.iter().any(|kept| kept.label == picked.label) {
                self.picked.push(picked);
            }
        }
    }
}

impl Widget for &Draft {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        self.area.render(area, buffer);
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

    /// Its text, with what each picked label stands for in its place.
    pub(crate) fn parts(&self) -> Vec<Part> {
        split(&self.text, &self.picked)
            .into_iter()
            .map(|part| match part {
                Split::Text(text) => Part::Text(text.to_owned()),
                Split::Picked(index) => {
                    let picked = &self.picked[index];
                    match &picked.reference {
                        Reference::Mention(Mentioned::Person(id)) => Part::User(id.clone()),
                        Reference::Mention(Mentioned::Broadcast(broadcast)) => {
                            Part::Broadcast(*broadcast)
                        }
                        Reference::Mention(Mentioned::UserGroup(id)) => Part::UserGroup(id.clone()),
                        Reference::Channel(id) => Part::Channel(id.clone()),
                        Reference::Link(url) => Part::Link {
                            url: url.clone(),
                            label: picked.label.clone(),
                        },
                    }
                }
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

/// A text field holding `text`, with the cursor at its end, showing `placeholder` while empty.
fn text_area(text: &str, placeholder: &'static str) -> TextArea<'static> {
    let mut area = TextArea::new(text.lines().map(str::to_owned).collect());
    area.set_cursor_line_style(Style::default());
    area.set_placeholder_text(placeholder);
    area.move_cursor(CursorMove::Bottom);
    area.move_cursor(CursorMove::End);
    area
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use slackers_model::ConversationId;

    use super::*;
    use crate::app::{ConversationRef, WorkspaceKey};

    fn draft(text: &str) -> Draft {
        let place = Place {
            conversation: ConversationRef {
                workspace: WorkspaceKey::nth(0),
                id: ConversationId::new("C1"),
            },
            thread: None,
        };
        let mut draft = Draft::new(&place);
        draft.set_text(text);
        draft
    }

    fn person(id: &str) -> Mentioned {
        Mentioned::Person(UserId::new(id))
    }

    fn sent(draft: &mut Draft) -> Vec<Part> {
        draft.take().expect("something to send").parts()
    }

    fn text(text: &str) -> Part {
        Part::Text(text.into())
    }

    fn mention(name: &str) -> Option<Completing> {
        Some(Completing::Mention(name.into()))
    }

    fn emoji(name: &str) -> Option<Completing> {
        Some(Completing::Emoji(name.into()))
    }

    #[test]
    fn a_word_starting_with_an_at_is_a_mention_being_written() {
        assert_eq!(draft("hi @ad").completing(), mention("ad"));
        assert_eq!(draft("@").completing(), mention(""));
        assert_eq!(draft("first\n@o'bri").completing(), mention("o'bri"));
        for not_one in ["hi", "hi @ada ", "ada@example.com", "@ada,", "(@ada"] {
            assert_eq!(draft(not_one).completing(), None, "{not_one}");
        }
        // It's the word before the cursor that counts.
        let mut moved = draft("@ada and more");
        moved.area.move_cursor(CursorMove::Head);
        for _ in 0.."@ad".len() {
            moved.area.move_cursor(CursorMove::Forward);
        }
        assert_eq!(moved.completing(), mention("ad"));
    }

    #[test]
    fn a_word_starting_with_a_colon_and_two_more_is_an_emoji_being_written() {
        assert_eq!(draft("nice :ta").completing(), emoji("ta"));
        assert_eq!(draft(":+1").completing(), emoji("+1"));
        assert_eq!(draft(":it's-a_ok").completing(), emoji("it's-a_ok"));
        assert_eq!(draft(":お疲").completing(), emoji("お疲"));
        for not_one in [
            ":", ":D", ":-)", "12:30", "note:ab", ":tada:", ":tada: ", "::std",
        ] {
            assert_eq!(draft(not_one).completing(), None, "{not_one}");
        }
    }

    #[test]
    fn a_slash_and_a_word_starting_a_draft_is_a_command() {
        let command = |name: &str| Some(Completing::Command(name.into()));
        assert_eq!(draft("/").completing(), command(""));
        assert_eq!(draft("/gi").completing(), command("gi"));
        assert_eq!(draft("/my-app_x").completing(), command("my-app_x"));
        for not_one in ["hi /gi", "/giphy cats", "first\n/gi", "//x", "/a.b"] {
            assert_eq!(draft(not_one).completing(), None, "{not_one}");
        }

        let mut written = draft("/giphy cats and @");
        written.pick(person("U1"), "Ada");
        assert_eq!(
            written.take().unwrap().command(),
            Some((
                "/giphy".to_owned(),
                vec![text("cats and "), Part::User(UserId::new("U1")), text(" ")]
            ))
        );
        assert_eq!(
            draft("/dnd").take().unwrap().command(),
            Some(("/dnd".to_owned(), Vec::new()))
        );
        for text in [" /giphy cats", "/ x", "//x", "a/b"] {
            assert_eq!(draft(text).take().unwrap().command(), None, "{text}");
        }
    }

    #[test]
    fn picking_an_emoji_writes_its_name_between_colons_as_slack_reads_it() {
        let mut draft = draft("nice :ta");
        draft.pick_emoji("tada");
        assert_eq!(draft.text(), "nice :tada: ");
        assert_eq!(draft.completing(), None);
        draft.pick_emoji("tada");
        assert_eq!(draft.text(), "nice :tada: ", "nothing to complete");
        assert_eq!(sent(&mut draft), [text("nice :tada: ")]);
    }

    #[test]
    fn picking_writes_the_label_and_sending_turns_it_into_the_mention() {
        let mut draft = draft("thanks @ad");
        draft.pick(person("U1"), "Ada Lovelace");
        draft.input(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(draft.text(), "thanks @Ada Lovelace !");
        assert_eq!(draft.completing(), None);
        assert_eq!(draft.mentions().collect::<Vec<_>>(), ["@Ada Lovelace"]);
        assert_eq!(
            sent(&mut draft),
            [text("thanks "), Part::User(UserId::new("U1")), text(" !")]
        );
        assert!(draft.is_empty() && draft.mentions().next().is_none());
    }

    #[test]
    fn only_whole_labels_starting_a_word_are_mentions() {
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        let typed = draft.text() + "x@Ada @Adam @Ada_ @Ada, @Ada";
        draft.set_text(&typed);
        assert_eq!(
            sent(&mut draft),
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
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        draft.area.insert_str("@");
        draft.pick(person("U2"), "Ada Lovelace");
        draft.area.insert_str("@");
        draft.pick(person("U3"), "Ada");
        draft.area.insert_str("@");
        draft.pick(person("U1"), "Ada");
        assert_eq!(draft.text(), "@Ada @Ada Lovelace @Ada (2) @Ada ");
        let mentioned: Vec<Part> = sent(&mut draft)
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
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        draft.area.insert_str("@");
        draft.pick(Mentioned::Broadcast(Broadcast::Here), "here");
        draft.set_text("@Ada, @her");
        assert_eq!(draft.mentions().collect::<Vec<_>>(), ["@Ada"]);
        assert_eq!(
            sent(&mut draft),
            [Part::User(UserId::new("U1")), text(", @her")]
        );
    }

    #[test]
    fn channels_groups_and_labelled_links_post_as_what_their_labels_stand_for() {
        let mut draft = draft("");
        draft.push_text("see ");
        draft.push_reference("#general", Reference::Channel(ConversationId::new("C1")));
        draft.push_text(" (");
        let docs = Reference::Link("https://example.com/docs".into());
        draft.push_reference("the docs", docs.clone());
        draft.push_text("), ");
        let group = Reference::Mention(Mentioned::UserGroup("S1".into()));
        draft.push_reference("@group", group);
        draft.push_text(" and ");
        draft.push_reference(
            "the docs",
            Reference::Link("https://example.com/other".into()),
        );
        assert_eq!(
            draft.text(),
            "see #general (the docs), @group and the docs (2)",
            "another link by the same label is told apart"
        );
        assert_eq!(
            draft.mentions().collect::<Vec<_>>(),
            ["@group"],
            "only those notified"
        );
        assert_eq!(
            sent(&mut draft),
            [
                text("see "),
                Part::Channel(ConversationId::new("C1")),
                text(" ("),
                Part::Link {
                    url: "https://example.com/docs".into(),
                    label: "the docs".into()
                },
                text("), "),
                Part::UserGroup("S1".into()),
                text(" and "),
                Part::Link {
                    url: "https://example.com/other".into(),
                    label: "the docs (2)".into()
                },
            ]
        );
    }

    #[test]
    fn what_wasnt_sent_goes_back_ahead_of_what_was_written_since() {
        let mut draft = draft("@");
        draft.pick(person("U1"), "Ada");
        let written = draft.take().unwrap();
        assert!(draft.take().is_none(), "nothing left to send");
        draft.set_text("later");
        draft.put_back(written);
        assert_eq!(draft.lines(), ["@Ada ", "later"]);
        assert_eq!(draft.mentions().collect::<Vec<_>>(), ["@Ada"]);
    }
}
