use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::{Emoji, Message, Reaction, ThreadSummary, Ts};

/// The Messages slackers holds for one Conversation, or for one Thread in it: oldest first, at most one per
/// [`Ts`].
///
/// History arrives in pages, and live Messages as they're posted; a `Timeline` merges both. A Conversation's
/// Timeline ([`Timeline::default`]) holds its top-level Messages and counts Replies on their parent's
/// [`ThreadSummary`]. A Thread's ([`Timeline::thread`]) holds the parent and its Replies.
#[derive(Clone, Debug)]
pub struct Timeline {
    messages: BTreeMap<Ts, Message>,
    /// Whether Slack has older Messages than the oldest one held.
    has_older: bool,
    /// The Thread held, by its parent; `None` for a Conversation's top-level Messages.
    thread: Option<Ts>,
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
    /// An empty Timeline of a Conversation's top-level Messages, whose history hasn't been loaded yet.
    fn default() -> Self {
        Self {
            messages: BTreeMap::new(),
            has_older: true,
            thread: None,
        }
    }
}

impl Timeline {
    /// An empty Timeline of the Thread started by the Message at `parent`, not loaded yet.
    pub fn thread(parent: Ts) -> Self {
        Self {
            messages: BTreeMap::new(),
            has_older: true,
            thread: Some(parent),
        }
    }

    /// Adds a Message posted live. In a Conversation's Timeline, a Reply instead counts on its parent's
    /// [`ThreadSummary`], when the parent is held and the Reply is newer than those counted. A Message from
    /// elsewhere than this Timeline's Thread is ignored.
    pub fn insert(&mut self, message: Message) {
        match (self.thread, message.reply_to) {
            (None, None) => {
                self.messages.insert(message.ts, message);
            }
            (None, Some(parent)) => self.count_reply(parent, message.ts),
            (Some(thread), _) if message.ts == thread => {
                self.messages.insert(message.ts, message);
            }
            (Some(thread), Some(parent)) if parent == thread => {
                let ts = message.ts;
                if let Entry::Vacant(entry) = self.messages.entry(ts) {
                    entry.insert(message);
                    self.count_reply(parent, ts);
                }
            }
            (Some(_), _) => {}
        }
    }

    /// Counts a Reply at `reply` on the parent at `parent`, unless it isn't newer than those already counted: the
    /// same Reply can arrive twice, as an answer to posting it and live.
    fn count_reply(&mut self, parent: Ts, reply: Ts) {
        let Some(parent) = self.messages.get_mut(&parent) else {
            return;
        };
        match &mut parent.thread {
            Some(summary) if reply <= summary.latest => {}
            Some(summary) => {
                summary.replies += 1;
                summary.latest = reply;
            }
            none => {
                *none = Some(ThreadSummary {
                    replies: 1,
                    latest: reply,
                })
            }
        }
    }

    /// Replaces the Message with the same [`Ts`], if held: it was edited.
    pub fn replace(&mut self, message: Message) {
        if let Some(held) = self.messages.get_mut(&message.ts) {
            // An edit carries the Message's content; what's counted live stays.
            let thread = held.thread;
            *held = message;
            held.thread = held.thread.or(thread);
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

    /// The Message posted at `ts`, if held.
    pub fn get(&self, ts: Ts) -> Option<&Message> {
        self.messages.get(&ts)
    }

    /// When the oldest Message held was posted; `None` when none is.
    pub fn oldest(&self) -> Option<Ts> {
        self.messages.keys().next().copied()
    }

    /// When the newest Message held was posted; `None` when none is.
    pub fn newest(&self) -> Option<Ts> {
        self.messages.keys().next_back().copied()
    }

    /// The Message held just before the one at `ts`; `None` when there's none. `ts` needn't be held.
    pub fn before(&self, ts: Ts) -> Option<Ts> {
        self.messages.range(..ts).next_back().map(|(&ts, _)| ts)
    }

    /// The Message held just after the one at `ts`; `None` when there's none. `ts` needn't be held.
    pub fn after(&self, ts: Ts) -> Option<Ts> {
        use std::ops::Bound::{Excluded, Unbounded};
        self.messages
            .range((Excluded(ts), Unbounded))
            .next()
            .map(|(&ts, _)| ts)
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

    fn ts(seconds: u64) -> Ts {
        Ts::from_micros(seconds * 1_000_000)
    }

    fn message(seconds: u64, text: &str) -> Message {
        Message {
            ts: ts(seconds),
            author: Author::User(UserId::new("U1")),
            content: RichText::plain(text),
            reply_to: None,
            thread: None,
            reactions: Vec::new(),
            files: Vec::new(),
            edited: false,
        }
    }

    fn reply(seconds: u64, parent: u64) -> Message {
        Message {
            reply_to: Some(ts(parent)),
            ..message(seconds, "reply")
        }
    }

    fn held(timeline: &Timeline) -> Vec<Ts> {
        timeline.messages().map(|m| m.ts).collect()
    }

    #[test]
    fn history_and_live_messages_merge_in_order() {
        let mut timeline = Timeline::default();
        timeline.insert(message(30, "live"));
        timeline.add_history([message(20, "b"), message(10, "a")], false);
        assert_eq!(held(&timeline), [ts(10), ts(20), ts(30)]);
        assert!(!timeline.has_older());
        assert_eq!(
            (timeline.oldest(), timeline.newest()),
            (Some(ts(10)), Some(ts(30)))
        );
        assert_eq!(
            (timeline.before(ts(20)), timeline.after(ts(20))),
            (Some(ts(10)), Some(ts(30)))
        );
        assert_eq!(
            (timeline.before(ts(10)), timeline.after(ts(30))),
            (None, None)
        );
        assert_eq!(
            timeline.after(ts(15)),
            Some(ts(20)),
            "from between two Messages"
        );
    }

    #[test]
    fn a_live_edit_survives_a_late_history_page() {
        let mut timeline = Timeline::default();
        let edited = Message {
            edited: true,
            ..message(10, "edited")
        };
        timeline.insert(edited.clone());
        timeline.add_history([message(10, "original")], true);
        assert_eq!(timeline.get(ts(10)), Some(&edited));
    }

    #[test]
    fn an_edit_replaces_only_what_is_held() {
        let mut timeline = Timeline::default();
        timeline.insert(message(10, "parent"));
        timeline.insert(reply(11, 10));
        timeline.replace(Message {
            edited: true,
            ..message(10, "edited")
        });
        timeline.replace(message(12, "never held"));
        assert_eq!(held(&timeline), [ts(10)]);
        let parent = timeline.get(ts(10)).unwrap();
        assert!(parent.edited);
        assert_eq!(
            parent.thread.unwrap().replies,
            1,
            "the edit keeps the count"
        );
    }

    #[test]
    fn replies_count_on_their_parent_once_each() {
        let mut timeline = Timeline::default();
        timeline.insert(message(10, "parent"));
        timeline.insert(reply(11, 10));
        timeline.insert(reply(11, 10));
        timeline.insert(reply(12, 10));

        assert_eq!(held(&timeline), [ts(10)]);
        let thread = timeline.get(ts(10)).unwrap().thread.unwrap();
        assert_eq!((thread.replies, thread.latest), (2, ts(12)));
    }

    #[test]
    fn a_threads_timeline_holds_its_parent_and_replies_only() {
        let mut timeline = Timeline::thread(ts(10));
        timeline.add_history([message(10, "parent"), reply(11, 10)], false);
        timeline.insert(reply(12, 10));
        timeline.insert(reply(12, 10));
        timeline.insert(reply(13, 99));
        timeline.insert(message(14, "elsewhere"));

        assert_eq!(held(&timeline), [ts(10), ts(11), ts(12)]);
        assert_eq!(
            timeline.get(ts(10)).unwrap().thread.unwrap().replies,
            1,
            "counted live, once"
        );
    }

    #[test]
    fn reactions_count_up_and_down() {
        let mut timeline = Timeline::default();
        timeline.insert(message(10, "hi"));
        let wave = Emoji::named("wave");

        timeline.react(ts(10), &wave, false, ReactionChange::Added);
        timeline.react(ts(10), &wave, true, ReactionChange::Added);
        let reaction = &timeline.get(ts(10)).unwrap().reactions[0];
        assert_eq!((reaction.count, reaction.mine), (2, true));

        timeline.react(ts(10), &wave, true, ReactionChange::Removed);
        timeline.react(ts(10), &wave, false, ReactionChange::Removed);
        assert!(timeline.get(ts(10)).unwrap().reactions.is_empty());
    }
}
