//! The pictures slackers fetches and keeps in the terminal, by URL: Custom Emoji and avatars.

use std::collections::HashMap;

use slackers_gfx::Picture;

/// Pictures by the URL they come from: loaded, on their way, or unavailable.
#[derive(Debug, Default)]
pub(crate) struct Pictures {
    by_url: HashMap<String, Loading>,
}

#[derive(Debug)]
enum Loading {
    Pending,
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
            Some(Loading::Pending) => PictureState::Awaiting { requested: true },
            Some(Loading::Failed) => PictureState::Failed,
            None => PictureState::Awaiting { requested: false },
        }
    }

    /// Notes that the picture at `url` is being fetched. Returns whether it wasn't already: whether to fetch it.
    pub(crate) fn request(&mut self, url: &str) -> bool {
        if self.by_url.contains_key(url) {
            return false;
        }
        self.by_url.insert(url.to_owned(), Loading::Pending);
        true
    }

    /// Whether the picture at `url` is on its way.
    pub(crate) fn awaits(&self, url: &str) -> bool {
        matches!(self.by_url.get(url), Some(Loading::Pending))
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
        assert!(pictures.request("u"));
        assert!(!pictures.request("u"));
        assert!(pictures.awaits("u"));
        assert_eq!(
            pictures.state("u"),
            PictureState::Awaiting { requested: true }
        );
        pictures.failed("u".into());
        assert_eq!(pictures.state("u"), PictureState::Failed);
        assert!(!pictures.request("u"), "not asked for again");

        pictures.forget_all();
        assert!(pictures.request("u"), "asked for again once forgotten");
    }
}
