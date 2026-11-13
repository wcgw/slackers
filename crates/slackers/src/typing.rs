//! Who's typing where, as Slack says live.

use std::collections::HashMap;
use std::time::Duration;

use slackers_model::UserId;

use crate::app::Place;

/// How long someone counts as typing after Slack last said they were: its clients say so every few seconds while
/// their user types, and nothing once they stop.
pub(crate) const TYPING_FOR: Duration = Duration::from_secs(6);

/// Who's typing where, each until [`TYPING_FOR`] after Slack last said so: the app asks to hear when that is, and
/// tells [`Typing::stopped`].
#[derive(Debug, Default)]
pub(crate) struct Typing {
    /// Who's typing, by where, in the order they started, with the time Slack last said so, as counted by `heard`.
    by_place: HashMap<Place, Vec<(UserId, u64)>>,
    /// How many times Slack said someone was typing.
    heard: u64,
}

impl Typing {
    /// Notes that `user` is typing at `place`, as Slack just said. Returns what to tell [`Typing::stopped`] once
    /// [`TYPING_FOR`] has passed.
    pub(crate) fn heard(&mut self, place: Place, user: UserId) -> u64 {
        self.heard += 1;
        let typists = self.by_place.entry(place).or_default();
        match typists.iter_mut().find(|(who, _)| *who == user) {
            Some((_, was)) => *was = self.heard,
            None => typists.push((user, self.heard)),
        }
        self.heard
    }

    /// [`TYPING_FOR`] has passed since Slack said, as `heard`, that `user` was typing at `place`: unless it said so
    /// again since, they stopped.
    pub(crate) fn stopped(&mut self, place: &Place, user: &UserId, heard: u64) {
        if let Some(typists) = self.by_place.get_mut(place) {
            typists.retain(|(who, when)| who != user || *when != heard);
            if typists.is_empty() {
                self.by_place.remove(place);
            }
        }
    }

    /// Notes that `user` posted at `place`: they're done typing there.
    pub(crate) fn posted(&mut self, place: &Place, user: &UserId) {
        if let Some(typists) = self.by_place.get_mut(place) {
            typists.retain(|(who, _)| who != user);
        }
    }

    /// Who's typing at `place`, in the order they started.
    pub(crate) fn at(&self, place: &Place) -> Vec<&UserId> {
        self.by_place
            .get(place)
            .into_iter()
            .flatten()
            .map(|(who, _)| who)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use slackers_model::{ConversationId, Ts};

    use super::*;
    use crate::app::{ConversationRef, WorkspaceKey};

    fn place(thread: Option<u64>) -> Place {
        Place {
            conversation: ConversationRef {
                workspace: WorkspaceKey::nth(0),
                id: ConversationId::new("C1"),
            },
            thread: thread.map(|s| Ts::from_micros(s * 1_000_000)),
        }
    }

    fn who(typing: &Typing, place: &Place) -> Vec<String> {
        typing
            .at(place)
            .into_iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn typing_lasts_until_slack_stops_saying_so() {
        let mut typing = Typing::default();
        let (ada, bob) = (UserId::new("U1"), UserId::new("U2"));
        let first = typing.heard(place(None), ada.clone());
        let second = typing.heard(place(None), bob.clone());
        assert_eq!(who(&typing, &place(None)), ["U1", "U2"]);
        assert!(who(&typing, &place(Some(5))).is_empty(), "not in a Thread");

        let again = typing.heard(place(None), ada.clone());
        typing.stopped(&place(None), &ada, first);
        assert_eq!(
            who(&typing, &place(None)),
            ["U1", "U2"],
            "U1 again, in their place"
        );
        typing.stopped(&place(None), &bob, second);
        assert_eq!(who(&typing, &place(None)), ["U1"]);
        typing.stopped(&place(None), &ada, again);
        assert!(who(&typing, &place(None)).is_empty());
        assert!(typing.by_place.is_empty(), "nothing piles up");
    }

    #[test]
    fn posting_ends_typing_where_posted() {
        let mut typing = Typing::default();
        typing.heard(place(None), UserId::new("U1"));
        typing.heard(place(Some(5)), UserId::new("U1"));
        typing.posted(&place(Some(5)), &UserId::new("U1"));
        assert!(who(&typing, &place(Some(5))).is_empty());
        assert_eq!(who(&typing, &place(None)), ["U1"]);
    }
}
