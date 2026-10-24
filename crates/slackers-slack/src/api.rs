//! Slack's Web API, called as the web client calls it: a form `POST` carrying the session token, with the
//! session cookies alongside.

use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, COOKIE, RETRY_AFTER};
use serde::de::DeserializeOwned;
use serde_json::Value;
use slackers_model::{ConversationId, CustomEmojiSet, Message, Ts, UserId};
use tokio::sync::Semaphore;

use crate::session::{HistoryPage, Snapshot, User};
use crate::wire::{AuthTest, Counts, EmojiList, History, Posted, UserBoot, UserInfo};
use crate::{Credentials, Error};

const BASE: &str = "https://slack.com/api";

/// How many calls may be in flight at once, so that a burst of lookups doesn't trip Slack's rate limits.
const IN_FLIGHT: usize = 6;

/// How many times a rate-limited call is retried, after waiting as long as Slack asks.
const RATE_LIMIT_RETRIES: u32 = 3;

/// A signed-in client of one Workspace's Web API.
#[derive(Debug)]
pub(crate) struct Api {
    http: reqwest::Client,
    credentials: Credentials,
    in_flight: Semaphore,
}

impl Api {
    pub(crate) fn new(credentials: Credentials) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("slackers/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            credentials,
            in_flight: Semaphore::new(IN_FLIGHT),
        })
    }

    /// Calls `method` with `params`, waiting out rate limits, and decodes its reply as `T`.
    async fn call<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: &[(&str, &str)],
    ) -> Result<T, Error> {
        let _permit = self
            .in_flight
            .acquire()
            .await
            .expect("the semaphore is never closed");
        let mut form = vec![("token", self.credentials.token())];
        form.extend_from_slice(params);
        for _ in 0..=RATE_LIMIT_RETRIES {
            let response = self
                .http
                .post(format!("{BASE}/{method}"))
                .header(COOKIE, self.credentials.cookies())
                .form(&form)
                .send()
                .await?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                let wait = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()?.parse().ok())
                    .unwrap_or(5);
                tracing::info!(method, wait, "rate limited by slack");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            let bytes = response.bytes().await?;
            let reply: Value = serde_json::from_slice(&bytes)
                .map_err(|source| Error::Unexpected { method, source })?;
            if reply["ok"] != Value::Bool(true) {
                return Err(Error::refused(
                    method,
                    reply["error"].as_str().unwrap_or("unknown"),
                ));
            }
            return serde_json::from_value(reply)
                .map_err(|source| Error::Unexpected { method, source });
        }
        Err(Error::refused(method, "ratelimited"))
    }

    /// Signs in and lists the user's Conversations with their Activity.
    pub(crate) async fn snapshot(&self) -> Result<Snapshot, Error> {
        let auth: AuthTest = self.call("auth.test", &[]).await?;
        let (boot, counts): (UserBoot, Counts) = tokio::try_join!(
            self.call("client.userBoot", &[]),
            self.call("client.counts", &[])
        )?;
        Ok(crate::wire::snapshot(auth, boot, counts))
    }

    /// Up to a page of Messages posted before `before` (or the newest ones, for `None`), newest first.
    pub(crate) async fn history(
        &self,
        conversation: &ConversationId,
        before: Option<Ts>,
        me: &UserId,
    ) -> Result<HistoryPage, Error> {
        let latest = before.map(|ts| ts.to_string()).unwrap_or_default();
        let mut params = vec![("channel", conversation.as_str()), ("limit", "100")];
        if before.is_some() {
            params.extend([("latest", latest.as_str()), ("inclusive", "false")]);
        }
        let history: History = self.call("conversations.history", &params).await?;
        Ok(history.into_page(me))
    }

    pub(crate) async fn user(&self, id: &UserId) -> Result<User, Error> {
        let info: UserInfo = self.call("users.info", &[("user", id.as_str())]).await?;
        Ok(info.into_user())
    }

    /// Posts `text` (see [`Request::Post`](crate::Request::Post)) to `conversation`, and returns the Message as
    /// Slack stored it.
    pub(crate) async fn post(
        &self,
        conversation: &ConversationId,
        text: &str,
        me: &UserId,
    ) -> Result<Message, Error> {
        let text = crate::mrkdwn::escape(text);
        let posted: Posted = self
            .call(
                "chat.postMessage",
                &[("channel", conversation.as_str()), ("text", &text)],
            )
            .await?;
        posted
            .into_message(me)
            .ok_or_else(|| Error::refused("chat.postMessage", "unreadable_message"))
    }

    /// Moves the Read Marker of `conversation` to `up_to`.
    pub(crate) async fn mark(&self, conversation: &ConversationId, up_to: Ts) -> Result<(), Error> {
        let up_to = up_to.to_string();
        let _: serde::de::IgnoredAny = self
            .call(
                "conversations.mark",
                &[("channel", conversation.as_str()), ("ts", &up_to)],
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn custom_emoji(&self) -> Result<CustomEmojiSet, Error> {
        let list: EmojiList = self.call("emoji.list", &[]).await?;
        Ok(list.into_set())
    }

    /// The bytes at `url`. Slack's own hosts get the session's credentials, as private files need; any other host,
    /// such as gravatar.com for avatars, gets none.
    pub(crate) async fn download(&self, url: &str) -> Result<Vec<u8>, Error> {
        let _permit = self
            .in_flight
            .acquire()
            .await
            .expect("the semaphore is never closed");
        let mut request = self.http.get(url);
        if is_slack(url) {
            request = request
                .bearer_auth(self.credentials.token())
                .header(COOKIE, self.credentials.cookies());
        }
        let response = request.send().await?.error_for_status()?;
        // Without valid credentials Slack answers 200 with its login page rather than an error.
        let is_login_page = is_slack(url)
            && response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/html"));
        if is_login_page {
            return Err(Error::SignedOut);
        }
        Ok(response.bytes().await?.to_vec())
    }
}

/// Whether `url` is served over HTTPS by Slack itself, and may therefore see the session's credentials.
fn is_slack(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default();
    url.scheme() == "https"
        && ["slack.com", "slack-edge.com"].iter().any(|domain| {
            host == *domain
                || host
                    .strip_suffix(domain)
                    .is_some_and(|sub| sub.ends_with('.'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_slack_sees_the_credentials() {
        for url in [
            "https://files.slack.com/files-pri/T1-F1/x.png",
            "https://emoji.slack-edge.com/T1/parrot/abc.gif",
            "https://slack.com/api/x",
        ] {
            assert!(is_slack(url), "{url}");
        }
        for url in [
            "https://secure.gravatar.com/avatar/x.jpg",
            "https://evilslack.com/x",
            "https://slack.com.evil.org/x",
            "http://files.slack.com/x",
            "not a url",
        ] {
            assert!(!is_slack(url), "{url}");
        }
    }
}
