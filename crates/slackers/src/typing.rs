//! Who's typing where, as Slack says live.

use std::collections::HashMap;
use std::time::Duration;

use slackers_model::UserId;

use crate::app::Place;

/// How long someone counts as typing after Slack last said they were: its clients say so every few seconds while
/// their user types, and nothing once they stop.
const TYPING_FOR: Duration = Duration::from_secs(6);

/// Who's typing where, each until a while after Slack last said so. Times are animation time.
#[derive(Debug, Default)]
pub(crate) struct Typing {
    /// Who's typing, by where, in the order they started, with when each stops counting as typing.
    by_place: HashMap<Place, Vec<(UserId, Duration)>>,
}

/// Who's typing somewhere.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Typists<'a> {
    /// Who, in the order they started.
    pub(crate) who: Vec<&'a UserId>,
    /// When the first of them stops counting as typing, unless Slack says otherwise first; `None` when nobody's
    /// typing.
    pub(crate) changes: Option<Duration>,
}

impl Typing {
    /// Notes that `user` is typing at `place`, as Slack said at `now`.
    pub(crate) fn heard(&mut self, place: Place, user: UserId, now: Duration) {
        // Forget whoever stopped, anywhere, so that nothing piles up.
        self.by_place.retain(|_, typists| {
            typists.retain(|&(_, until)| until > now);
            !typists.is_empty()
        });
        let until = now + TYPING_FOR;
        let typists = self.by_place.entry(place).or_default();
        match typists.iter_mut().find(|(who, _)| *who == user) {
            Some((_, was)) => *was = until,
            None => typists.push((user, until)),
        }
    }

    /// Notes that `user` posted at `place`: they're done typing there.
    pub(crate) fn posted(&mut self, place: &Place, user: &UserId) {
        if let Some(typists) = self.by_place.get_mut(place) {
            typists.retain(|(who, _)| who != user);
        }
    }

    /// Who's typing at `place` as of `now`.
    pub(crate) fn at(&self, place: &Place, now: Duration) -> Typists<'_> {
        let typing = || {
            self.by_place
                .get(place)
                .into_iter()
                .flatten()
                .filter(move |&&(_, until)| until > now)
        };
        Typists {
            who: typing().map(|(who, _)| who).collect(),
            changes: typing().map(|&(_, until)| until).min(),
        }
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

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn who(typing: &Typing, place: &Place, now: Duration) -> Vec<String> {
        typing
            .at(place, now)
            .who
            .into_iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn typing_lasts_a_while_after_slack_last_says_so() {
        let mut typing = Typing::default();
        typing.heard(place(None), UserId::new("U1"), secs(10));
        typing.heard(place(None), UserId::new("U2"), secs(12));
        assert_eq!(who(&typing, &place(None), secs(12)), ["U1", "U2"]);
        assert_eq!(typing.at(&place(None), secs(12)).changes, Some(secs(16)));
        assert!(
            who(&typing, &place(Some(5)), secs(12)).is_empty(),
            "not in a Thread"
        );

        typing.heard(place(None), UserId::new("U1"), secs(15));
        assert_eq!(
            who(&typing, &place(None), secs(17)),
            ["U1", "U2"],
            "U1 again, in their place"
        );
        assert_eq!(who(&typing, &place(None), secs(18)), ["U1"]);
        assert_eq!(typing.at(&place(None), secs(18)).changes, Some(secs(21)));
        assert_eq!(
            typing.at(&place(None), secs(21)),
            Typists {
                who: Vec::new(),
                changes: None
            }
        );
    }

    #[test]
    fn posting_ends_typing_where_posted() {
        let mut typing = Typing::default();
        typing.heard(place(None), UserId::new("U1"), secs(1));
        typing.heard(place(Some(5)), UserId::new("U1"), secs(1));
        typing.posted(&place(Some(5)), &UserId::new("U1"));
        assert!(who(&typing, &place(Some(5)), secs(2)).is_empty());
        assert_eq!(who(&typing, &place(None), secs(2)), ["U1"]);
    }

    #[test]
    fn whoever_stopped_is_forgotten() {
        let mut typing = Typing::default();
        typing.heard(place(None), UserId::new("U1"), secs(1));
        typing.heard(place(Some(5)), UserId::new("U2"), secs(100));
        assert_eq!(typing.by_place.len(), 1);
    }
}
