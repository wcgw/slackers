//! The pictures slackers fetches and keeps in the terminal, by URL: Custom Emoji, avatars, Workspace icons and
//! images shared in Messages.

use std::collections::HashMap;

use slackers_gfx::{Fit, Picture};

/// Pictures by the URL they come from: loaded, on their way, or unavailable.
#[derive(Debug, Default)]
pub(crate) struct Pictures {
    by_url: HashMap<String, Loading>,
}

#[derive(Debug)]
enum Loading {
    /// On its way, to be drawn at this size.
    Pending(Fit),
    Ready(Picture),
    Failed,
}

/// Where the picture from one URL stands.
#[derive(Debug, PartialEq)]
pub(crate) enum PictureState<'a> {
    /// Loaded, and in the terminal.
    Ready(&'a Picture),
    /// Not loaded; it's on its way when `requested`, else nobody has asked for it yet.
    Awaiting { requested: bool },
    /// Couldn't be loaded; draw something else.
    Failed,
}

impl Pictures {
    pub(crate) fn state(&self, url: &str) -> PictureState<'_> {
        match self.by_url.get(url) {
            Some(Loading::Ready(picture)) => PictureState::Ready(picture),
            Some(Loading::Pending(_)) => PictureState::Awaiting { requested: true },
            Some(Loading::Failed) => PictureState::Failed,
            None => PictureState::Awaiting { requested: false },
        }
    }

    /// Notes that the picture at `url` is being fetched, to be drawn to `fit`. Returns whether it wasn't already:
    /// whether to fetch it.
    pub(crate) fn request(&mut self, url: &str, fit: Fit) -> bool {
        if self.by_url.contains_key(url) {
            return false;
        }
        self.by_url.insert(url.to_owned(), Loading::Pending(fit));
        true
    }

    /// How to draw the picture at `url`, if it's on its way; `None` if it isn't.
    pub(crate) fn awaited(&self, url: &str) -> Option<Fit> {
        match self.by_url.get(url) {
            Some(Loading::Pending(fit)) => Some(*fit),
            Some(Loading::Ready(_) | Loading::Failed) | None => None,
        }
    }

    pub(crate) fn loaded(&mut self, url: String, picture: Picture) {
        self.by_url.insert(url, Loading::Ready(picture));
    }

    /// Notes that the picture at `url` couldn't be loaded, so that it isn't asked for again.
    pub(crate) fn failed(&mut self, url: String) {
        self.by_url.insert(url, Loading::Failed);
    }

    /// Forgets every picture, loaded or not, so that each is fetched again when next drawn: the terminal no longer
    /// has them.
    pub(crate) fn forget_all(&mut self) {
        self.by_url.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_requested_once_and_failures_stick() {
        let mut pictures = Pictures::default();
        assert_eq!(
            pictures.state("u"),
            PictureState::Awaiting { requested: false }
        );
        assert!(pictures.request("u", Fit::Emoji));
        assert!(!pictures.request("u", Fit::Emoji));
        assert_eq!(pictures.awaited("u"), Some(Fit::Emoji));
        assert_eq!(
            pictures.state("u"),
            PictureState::Awaiting { requested: true }
        );
        pictures.failed("u".into());
        assert_eq!(pictures.state("u"), PictureState::Failed);
        assert!(!pictures.request("u", Fit::Emoji), "not asked for again");
        assert_eq!(pictures.awaited("u"), None);

        pictures.forget_all();
        assert!(
            pictures.request("u", Fit::Emoji),
            "asked for again once forgotten"
        );
    }
}
