//! Files, images, avatars and Custom Emoji, fetched for the app to show.

use std::sync::Arc;

use crate::api::Api;
use crate::{Credentials, Error};

/// Fetches what a Workspace's Messages show, with the Workspace's credentials where Slack needs them. Clones share
/// one client, and its limit on calls in flight.
#[derive(Clone, Debug)]
pub struct Downloader {
    api: Arc<Api>,
}

impl Downloader {
    /// A downloader signing its requests to Slack with `credentials`.
    ///
    /// # Errors
    ///
    /// When the HTTP client can't be set up.
    pub fn new(credentials: Credentials) -> Result<Self, Error> {
        Ok(Self {
            api: Arc::new(Api::new(credentials)?),
        })
    }

    /// The bytes at `url`. Slack's own hosts get the credentials, as private files need; any other host, such as
    /// gravatar.com for avatars, gets none.
    ///
    /// # Errors
    ///
    /// When Slack can't be reached or refuses, or the credentials no longer work.
    pub async fn download(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.api.download(url).await
    }
}
