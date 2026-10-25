//! The pictures slackers fetches and keeps in the terminal, by URL: Custom Emoji, avatars, Workspace icons and
//! images shared in Messages. Those off screen are forgotten once they take too much of the terminal's memory.

use std::collections::HashMap;

use slackers_gfx::{Fit, Picture};

/// How much of the terminal's memory loaded pictures may take, in bytes, before those off screen are forgotten.
///
/// Ghostty keeps 320 MB of pictures by default, and past that drops the oldest, on screen or not. This leaves room
/// for whatever is on screen on top, which is never forgotten.
const BUDGET: usize = 128 << 20;

/// Pictures by the URL they come from: loaded, on their way, or unavailable.
#[derive(Debug)]
pub(crate) struct Pictures {
    by_url: HashMap<String, Loading>,
    /// How much of the terminal's memory the loaded pictures take, in bytes.
    stored: usize,
    /// How much they may take before those off screen are forgotten.
    budget: usize,
    /// How many draws have ended; see [`Pictures::evict`].
    draws: u64,
}

#[derive(Debug)]
enum Loading {
    /// On its way, to be drawn at this size.
    Pending(Fit),
    /// In the terminal, and last wanted by the draw numbered `wanted`.
    Ready {
        picture: Picture,
        wanted: u64,
    },
    Failed,
}

/// Where the picture from one URL stands.
#[derive(Debug, PartialEq)]
pub(crate) enum PictureState<'a> {
    /// Loaded, and in the terminal.
    Ready(&'a Picture),
    /// Not loaded yet.
    Awaiting,
    /// Couldn't be loaded; draw something else.
    Failed,
}

impl Default for Pictures {
    fn default() -> Self {
        Self::with_budget(BUDGET)
    }
}

impl Pictures {
    /// Pictures that may take `budget` bytes of the terminal's memory before those off screen are forgotten.
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            by_url: HashMap::new(),
            stored: 0,
            budget,
            draws: 0,
        }
    }

    pub(crate) fn state(&self, url: &str) -> PictureState<'_> {
        match self.by_url.get(url) {
            Some(Loading::Ready { picture, .. }) => PictureState::Ready(picture),
            Some(Loading::Pending(_)) | None => PictureState::Awaiting,
            Some(Loading::Failed) => PictureState::Failed,
        }
    }

    /// Notes that the draw under way shows the picture at `url`, or would once it's loaded, drawn to `fit`. Returns
    /// whether to fetch it: whether it's neither loaded, nor on its way, nor known to fail.
    pub(crate) fn want(&mut self, url: &str, fit: Fit) -> bool {
        match self.by_url.get_mut(url) {
            Some(Loading::Ready { wanted, .. }) => *wanted = self.draws,
            Some(Loading::Pending(_) | Loading::Failed) => {}
            None => {
                self.by_url.insert(url.to_owned(), Loading::Pending(fit));
                return true;
            }
        }
        false
    }

    /// Ends the draw under way. Takes out the loaded pictures it didn't want, least recently wanted first, for as
    /// long as those loaded take more than the budget: the terminal should forget them. Each is fetched again
    /// when next wanted.
    pub(crate) fn evict(&mut self) -> Vec<Picture> {
        let ended = self.draws;
        self.draws += 1;
        if self.stored <= self.budget {
            return Vec::new();
        }
        let mut idle: Vec<(u64, String)> = self
            .by_url
            .iter()
            .filter_map(|(url, loading)| match loading {
                Loading::Ready { wanted, .. } => (*wanted < ended).then(|| (*wanted, url.clone())),
                Loading::Pending(_) | Loading::Failed => None,
            })
            .collect();
        idle.sort_unstable();
        let mut evicted = Vec::new();
        for (_, url) in idle {
            if self.stored <= self.budget {
                break;
            }
            if let Some(Loading::Ready { picture, .. }) = self.by_url.remove(&url) {
                self.stored -= picture.stored_bytes();
                evicted.push(picture);
            }
        }
        evicted
    }

    /// How to draw the picture at `url`, if it's on its way; `None` if it isn't.
    pub(crate) fn awaited(&self, url: &str) -> Option<Fit> {
        match self.by_url.get(url) {
            Some(Loading::Pending(fit)) => Some(*fit),
            Some(Loading::Ready { .. } | Loading::Failed) | None => None,
        }
    }

    /// Keeps `picture`, loaded from `url`, as wanted by the draw under way. Returns it back when one from `url` is
    /// loaded already, for the terminal to forget.
    pub(crate) fn loaded(&mut self, url: String, picture: Picture) -> Option<Picture> {
        if let Some(Loading::Ready { .. }) = self.by_url.get(&url) {
            return Some(picture);
        }
        self.stored += picture.stored_bytes();
        let wanted = self.draws;
        self.by_url.insert(url, Loading::Ready { picture, wanted });
        None
    }

    /// Notes that the picture at `url` couldn't be loaded, so that it isn't asked for again; unless it's loaded
    /// already, and this was about another copy.
    pub(crate) fn failed(&mut self, url: String) {
        if !matches!(self.by_url.get(&url), Some(Loading::Ready { .. })) {
            self.by_url.insert(url, Loading::Failed);
        }
    }

    /// Forgets every picture, loaded or not, so that each is fetched again when next drawn: the terminal no longer
    /// has them.
    pub(crate) fn forget_all(&mut self) {
        self.by_url.clear();
        self.stored = 0;
    }
}

#[cfg(test)]
mod tests {
    use slackers_gfx::Ids;

    use super::*;

    #[test]
    fn a_picture_is_requested_once_and_failures_stick() {
        let mut pictures = Pictures::default();
        assert_eq!(pictures.state("u"), PictureState::Awaiting);
        assert!(pictures.want("u", Fit::Emoji));
        assert!(!pictures.want("u", Fit::Emoji));
        assert_eq!(pictures.awaited("u"), Some(Fit::Emoji));
        assert_eq!(pictures.state("u"), PictureState::Awaiting);
        pictures.failed("u".into());
        assert_eq!(pictures.state("u"), PictureState::Failed);
        assert!(!pictures.want("u", Fit::Emoji), "not asked for again");
        assert_eq!(pictures.awaited("u"), None);

        pictures.forget_all();
        assert!(
            pictures.want("u", Fit::Emoji),
            "asked for again once forgotten"
        );
    }

    /// Loads a picture taking `bytes` from each of `urls`.
    fn load(pictures: &mut Pictures, ids: &Ids, urls: &[&str], bytes: usize) {
        for url in urls {
            assert!(pictures.want(url, Fit::Emoji));
            let picture = Picture::for_tests(ids, 1, bytes);
            assert_eq!(pictures.loaded((*url).to_owned(), picture), None);
        }
    }

    /// A draw that wants `urls`; what it evicts, by how many bytes each takes.
    fn draw(pictures: &mut Pictures, urls: &[&str]) -> Vec<usize> {
        for url in urls {
            assert!(!pictures.want(url, Fit::Emoji), "{url} is loaded");
        }
        let evicted = pictures.evict();
        evicted.iter().map(Picture::stored_bytes).collect()
    }

    #[test]
    fn over_budget_the_least_recently_drawn_go_but_never_those_drawn_last() {
        let ids = Ids::default();
        let mut pictures = Pictures::with_budget(100);
        load(&mut pictures, &ids, &["a", "b", "c"], 40);
        assert!(
            draw(&mut pictures, &["a", "b", "c"]).is_empty(),
            "120 bytes, all on screen"
        );
        load(&mut pictures, &ids, &["d"], 10);
        assert_eq!(draw(&mut pictures, &["c", "d"]), [40]);
        assert_eq!(
            pictures.state("a"),
            PictureState::Awaiting,
            "the oldest went"
        );
        assert!(
            matches!(pictures.state("b"), PictureState::Ready(_)),
            "back under budget"
        );
        assert!(draw(&mut pictures, &["c", "d"]).is_empty());

        load(&mut pictures, &ids, &["e"], 100);
        assert_eq!(draw(&mut pictures, &["d", "e"]), [40, 40], "b and c went");
        assert!(
            draw(&mut pictures, &["d", "e"]).is_empty(),
            "over budget, but on screen"
        );
        assert!(pictures.want("a", Fit::Emoji), "fetched again when wanted");
    }

    #[test]
    fn a_picture_loaded_twice_is_kept_once_and_those_forgotten_by_the_terminal_take_no_room() {
        let ids = Ids::default();
        let mut pictures = Pictures::with_budget(100);
        load(&mut pictures, &ids, &["a"], 60);
        let again = Picture::for_tests(&ids, 1, 60);
        assert_eq!(
            pictures.loaded("a".into(), again).map(|p| p.stored_bytes()),
            Some(60)
        );
        pictures.failed("a".into());
        assert!(
            matches!(pictures.state("a"), PictureState::Ready(_)),
            "another copy failed"
        );
        load(&mut pictures, &ids, &["b"], 30);
        // Pictures loaded during a draw go no sooner than the draw after.
        for _ in 0..2 {
            assert!(
                draw(&mut pictures, &[]).is_empty(),
                "90 bytes: the second a didn't count"
            );
        }

        pictures.forget_all();
        load(&mut pictures, &ids, &["c"], 90);
        for _ in 0..2 {
            assert!(
                draw(&mut pictures, &[]).is_empty(),
                "the forgotten ones don't count"
            );
        }
    }
}
