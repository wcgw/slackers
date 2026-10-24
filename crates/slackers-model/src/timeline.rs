use std::collections::BTreeMap;

use crate::{Emoji, Message, Reaction, ThreadSummary, Ts};

/// The top-level Messages of one Conversation that slackers holds, oldest first, at most one per [`Ts`].
///
/// History arrives in pages, newest first, and live Messages arrive as they're posted. A `Timeline` merges both,
/// and folds Replies into their parent's [`ThreadSummary`] rather than holding them.
#[derive(Clone, Debug)]
pub struct Timeline {
    messages: BTreeMap<Ts, Message>,
    /// Whether Slack has older Messages than the oldest one held.
    has_older: bool,
}

/// Whether a reaction was added or taken back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReactionChange {
    /// Someone reacted.
    Added,
    /// Someone took their reaction back.
    Removed,
}

impl Default for Timeline {
    /// An empty `Timeline`, whose history hasn't been loaded yet.
    fn default() -> Self {
        Self {
            messages: BTreeMap::new(),
            has_older: true,
        }
    }
}

impl Timeline {
    /// Adds a Message posted live, or replaces the one with the same [`Ts`] when it was edited. A Reply instead
    /// updates its parent's [`ThreadSummary`], if the parent is held.
    pub fn insert(&mut self, message: Message) {
        let Some(parent) = message.reply_to else {
            self.messages.insert(message.ts, message);
            return;
        };
        if let Some(parent) = self.messages.get_mut(&parent) {
            let summary = parent.thread.get_or_insert(ThreadSummary {
                replies: 0,
                latest: message.ts,
            });
            summary.replies += 1;
            summary.latest = summary.latest.max(message.ts);
        }
    }

    /// Adds a page of history. `has_more` says whether Slack has Messages older than the page.
    pub fn add_history(&mut self, page: impl IntoIterator<Item = Message>, has_more: bool) {
        for message in page {
            // A live edit may have arrived while the page was in flight: keep the live copy.
            self.messages.entry(message.ts).or_insert(message);
        }
        self.has_older = has_more;
    }

    /// Removes the Message posted at `ts`, if held.
    pub fn remove(&mut self, ts: Ts) {
        self.messages.remove(&ts);
    }

    /// Applies a reaction with `emoji` to the Message at `ts`, if held. `mine` says whether the user reacted.
    pub fn react(&mut self, ts: Ts, emoji: &Emoji, mine: bool, change: ReactionChange) {
        let Some(message) = self.messages.get_mut(&ts) else {
            return;
        };
        let existing = message
            .reactions
            .iter()
            .position(|reaction| reaction.emoji == *emoji);
        match (change, existing) {
            (ReactionChange::Added, Some(at)) => {
                let reaction = &mut message.reactions[at];
                reaction.count += 1;
                reaction.mine |= mine;
            }
            (ReactionChange::Added, None) => {
                message.reactions.push(Reaction {
                    emoji: emoji.clone(),
                    count: 1,
                    mine,
                });
            }
            (ReactionChange::Removed, Some(at)) => {
                let reaction = &mut message.reactions[at];
                reaction.count = reaction.count.saturating_sub(1);
                reaction.mine &= !mine;
                if reaction.count == 0 {
                    message.reactions.remove(at);
                }
            }
            (ReactionChange::Removed, None) => {}
        }
    }

    /// The Messages, oldest first.
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &Message> + ExactSizeIterator {
        self.messages.values()
    }

    /// When the oldest Message held was posted; `None` when none is.
    pub fn oldest(&self) -> Option<Ts> {
        self.messages.keys().next().copied()
    }

    /// Whether Slack has older Messages than those held.
    pub fn has_older(&self) -> bool {
        self.has_older
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Author, RichText, UserId};

    fn message(seconds: u64, text: &str) -> Message {
        Message {
            ts: Ts::from_micros(seconds * 1_000_000),
            author: Author::User(UserId::new("U1")),
            content: RichText::plain(text),
            reply_to: None,
            thread: None,
            reactions: Vec::new(),
            files: Vec::new(),
            edited: false,
        }
    }

    fn texts(timeline: &Timeline) -> Vec<String> {
        timeline
            .messages()
            .map(|m| format!("{:?}", m.content.inlines().next()))
            .collect()
    }

    #[test]
    fn history_and_live_messages_merge_in_order() {
        let mut timeline = Timeline::default();
        timeline.insert(message(30, "live"));
        timeline.add_history([message(20, "b"), message(10, "a")], false);
        let order: Vec<Ts> = timeline.messages().map(|m| m.ts).collect();
        assert_eq!(order, [10, 20, 30].map(|s| Ts::from_micros(s * 1_000_000)));
        assert!(!timeline.has_older());
        assert_eq!(timeline.oldest(), Some(Ts::from_micros(10_000_000)));
    }

    #[test]
    fn a_live_edit_survives_a_late_history_page() {
        let mut timeline = Timeline::default();
        let mut edited = message(10, "edited");
        edited.edited = true;
        timeline.insert(edited);
        timeline.add_history([message(10, "original")], true);
        assert_eq!(
            texts(&timeline),
            [format!("{:?}", RichText::plain("edited").inlines().next())]
        );
    }

    #[test]
    fn replies_count_on_their_parent() {
        let mut timeline = Timeline::default();
        timeline.insert(message(10, "parent"));
        let mut reply = message(11, "reply");
        reply.reply_to = Some(Ts::from_micros(10_000_000));
        timeline.insert(reply);

        assert_eq!(timeline.messages().len(), 1);
        let thread = timeline.messages().next().unwrap().thread.unwrap();
        assert_eq!(thread.replies, 1);
        assert_eq!(thread.latest, Ts::from_micros(11_000_000));
    }

    #[test]
    fn reactions_count_up_and_down() {
        let mut timeline = Timeline::default();
        timeline.insert(message(10, "hi"));
        let ts = Ts::from_micros(10_000_000);
        let wave = Emoji::named("wave");

        timeline.react(ts, &wave, false, ReactionChange::Added);
        timeline.react(ts, &wave, true, ReactionChange::Added);
        let reaction = &timeline.messages().next().unwrap().reactions[0];
        assert_eq!((reaction.count, reaction.mine), (2, true));

        timeline.react(ts, &wave, true, ReactionChange::Removed);
        timeline.react(ts, &wave, false, ReactionChange::Removed);
        assert!(timeline.messages().next().unwrap().reactions.is_empty());
    }
}
