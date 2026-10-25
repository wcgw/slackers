//! Slack's Web API, called as the web client calls it: a form `POST` carrying the session token, with the
//! session cookies alongside.

use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, COOKIE, RETRY_AFTER};
use reqwest::{RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use slackers_model::{
    Conversation, ConversationId, Emoji, Message, Reaction, ReactionChange, RichText, Ts, Unread,
    UserId, WorkspaceId,
};
use tokio::sync::Semaphore;

use crate::session::{
    ActivityCursor, ActivityPage, EmojiList, HistoryPage, Part, Person, Snapshot, User,
};
use crate::wire::{
    ACTIVITY_KINDS, ActivityFeed, AuthTest, ConversationInfo, Counts, History, ListedEmoji,
    PeopleSearch, Posted, Prefs, ReactedTo, Replies, ThreadView, Updated, UserBoot, UserInfo,
    reaction_name,
};
use crate::{Credentials, Error};

const BASE: &str = "https://slack.com/api";

/// Where the web client's searches go: Slack's edge API, addressed by Workspace.
const EDGE: &str = "https://edgeapi.slack.com/cache";

/// The most people one search finds.
const PEOPLE_FOUND: u32 = 25;

/// How many calls may be in flight at once, so that a burst of lookups doesn't trip Slack's rate limits.
const IN_FLIGHT: usize = 6;

/// The most Replies of one Thread fetched: enough for any Thread worth reading in a terminal.
const MAX_REPLIES: usize = 1000;

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
        let mut form = vec![("token", self.credentials.token())];
        form.extend_from_slice(params);
        self.answer(method, || {
            self.http
                .post(format!("{BASE}/{method}"))
                .header(COOKIE, self.credentials.cookies())
                .form(&form)
        })
        .await
    }

    /// Calls the edge API's `method` for `workspace` with `body`, waiting out rate limits, and decodes its reply as
    /// `T`.
    async fn call_edge<T: DeserializeOwned>(
        &self,
        workspace: &WorkspaceId,
        method: &'static str,
        body: &Value,
    ) -> Result<T, Error> {
        self.answer(method, || {
            self.http
                .post(format!("{EDGE}/{workspace}/{method}"))
                .bearer_auth(self.credentials.token())
                .header(COOKIE, self.credentials.cookies())
                .json(body)
        })
        .await
    }

    /// Sends what `request` builds for `method`, again after waiting as long as Slack asks while it's rate limited,
    /// and decodes Slack's reply as `T`.
    async fn answer<T: DeserializeOwned>(
        &self,
        method: &'static str,
        request: impl Fn() -> RequestBuilder,
    ) -> Result<T, Error> {
        let _permit = self
            .in_flight
            .acquire()
            .await
            .expect("the semaphore is never closed");
        for _ in 0..=RATE_LIMIT_RETRIES {
            let response = request().send().await?;
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

    /// Signs in and lists the user's Conversations and Followed Threads with what's Unread in each.
    pub(crate) async fn snapshot(&self) -> Result<Snapshot, Error> {
        let auth: AuthTest = self.call("auth.test", &[]).await?;
        let (boot, counts, threads) = tokio::join!(
            self.call::<UserBoot>("client.userBoot", &[]),
            self.call::<Counts>("client.counts", &[]),
            self.call::<ThreadView>("subscriptions.thread.getView", &[("limit", "50")]),
        );
        // Signing in can do without Followed Threads.
        let followed = match threads {
            Ok(view) => view.into_followed(),
            Err(error) => {
                tracing::warn!(%error, "couldn't list followed threads");
                Vec::new()
            }
        };
        Ok(crate::wire::snapshot(auth, boot?, counts?, followed))
    }

    /// Up to a page of Messages posted before `before` (or the newest ones, for `None`), newest first.
    pub(crate) async fn history(
        &self,
        conversation: &ConversationId,
        before: Option<Ts>,
    ) -> Result<HistoryPage, Error> {
        let latest = before.map(|ts| ts.to_string()).unwrap_or_default();
        let mut params = vec![("channel", conversation.as_str()), ("limit", "100")];
        if before.is_some() {
            params.extend([("latest", latest.as_str()), ("inclusive", "false")]);
        }
        let history: History = self.call("conversations.history", &params).await?;
        Ok(history.into_page())
    }

    /// The Conversation `id` as the sidebar lists it, with what's Unread in it as of its Read Marker; `None` when the
    /// sidebar wouldn't list it, being archived or not the user's.
    pub(crate) async fn conversation(
        &self,
        id: &ConversationId,
    ) -> Result<Option<(Conversation, Unread)>, Error> {
        let info: ConversationInfo = self
            .call("conversations.info", &[("channel", id.as_str())])
            .await?;
        Ok(info.into_listed())
    }

    pub(crate) async fn user(&self, id: &UserId) -> Result<User, Error> {
        let info: UserInfo = self.call("users.info", &[("user", id.as_str())]).await?;
        Ok(info.into_user())
    }

    /// Replaces the text of the user's Message at `ts` with `text`, and returns its new content as Slack stored it.
    pub(crate) async fn edit(
        &self,
        conversation: &ConversationId,
        ts: Ts,
        text: &[Part],
    ) -> Result<RichText, Error> {
        let (ts, text) = (ts.to_string(), crate::mrkdwn::write(text));
        let params = [
            ("channel", conversation.as_str()),
            ("ts", ts.as_str()),
            ("text", text.as_str()),
        ];
        let updated: Updated = self.call("chat.update", &params).await?;
        Ok(updated.into_content())
    }

    /// Posts `text` to `conversation`, and returns the Message as Slack stored it.
    pub(crate) async fn post(
        &self,
        conversation: &ConversationId,
        thread: Option<Ts>,
        text: &[Part],
    ) -> Result<Message, Error> {
        let text = crate::mrkdwn::write(text);
        let parent = thread.map(|ts| ts.to_string());
        let mut params = vec![("channel", conversation.as_str()), ("text", text.as_str())];
        if let Some(parent) = &parent {
            params.push(("thread_ts", parent));
        }
        let posted: Posted = self.call("chat.postMessage", &params).await?;
        posted
            .into_message()
            .ok_or_else(|| Error::refused("chat.postMessage", "unreadable_message"))
    }

    /// A page of the user's Activity, newest first, of the kinds slackers shows: the newest when `cursor` is `None`.
    pub(crate) async fn activity(
        &self,
        cursor: Option<&ActivityCursor>,
    ) -> Result<ActivityPage, Error> {
        let mut params = vec![
            ("mode", "chrono_reads_and_unreads"),
            ("types", ACTIVITY_KINDS),
            ("limit", "50"),
        ];
        if let Some(ActivityCursor(cursor)) = cursor {
            params.push(("cursor", cursor));
        }
        let feed: ActivityFeed = self.call("activity.feed", &params).await?;
        Ok(feed.into_page())
    }

    /// The Message at `ts` in `conversation`, a Reply or not; `None` if it's gone.
    pub(crate) async fn message(
        &self,
        conversation: &ConversationId,
        ts: Ts,
    ) -> Result<Option<Message>, Error> {
        let ts_text = ts.to_string();
        // Given a Reply's ts, `conversations.replies` answers for its Thread; the bounds keep it to the one.
        let params = [
            ("channel", conversation.as_str()),
            ("ts", ts_text.as_str()),
            ("oldest", ts_text.as_str()),
            ("latest", ts_text.as_str()),
            ("inclusive", "true"),
            ("limit", "1"),
        ];
        let replies: Replies = self.call("conversations.replies", &params).await?;
        Ok(replies.into_messages().find(|message| message.ts == ts))
    }

    /// Adds the user's reaction with `emoji` to the Message at `ts`, or takes it back, as `change` says. A reaction
    /// that's as asked already is fine.
    pub(crate) async fn react(
        &self,
        conversation: &ConversationId,
        ts: Ts,
        emoji: &Emoji,
        change: ReactionChange,
    ) -> Result<(), Error> {
        let method = match change {
            ReactionChange::Added => "reactions.add",
            ReactionChange::Removed => "reactions.remove",
        };
        let (ts, name) = (ts.to_string(), reaction_name(emoji));
        let params = [
            ("channel", conversation.as_str()),
            ("timestamp", ts.as_str()),
            ("name", name.as_str()),
        ];
        match self.call::<serde::de::IgnoredAny>(method, &params).await {
            Ok(_) => Ok(()),
            // Another client got there first, or this one did and its live event hasn't come yet.
            Err(Error::Refused { code, .. })
                if code == "already_reacted" || code == "no_reaction" =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Every reaction to the Message at `ts`, naming everyone who reacted.
    pub(crate) async fn reactions(
        &self,
        conversation: &ConversationId,
        ts: Ts,
    ) -> Result<Vec<Reaction>, Error> {
        let ts = ts.to_string();
        let params = [
            ("channel", conversation.as_str()),
            ("timestamp", ts.as_str()),
            ("full", "true"),
        ];
        let reacted: ReactedTo = self.call("reactions.get", &params).await?;
        reacted
            .into_reactions()
            .ok_or_else(|| Error::refused("reactions.get", "unreadable_message"))
    }

    /// The Thread started by the Message at `parent`: the parent first, then its Replies, oldest first. Stops after
    /// [`MAX_REPLIES`] Replies.
    pub(crate) async fn replies(
        &self,
        conversation: &ConversationId,
        parent: Ts,
    ) -> Result<Vec<Message>, Error> {
        let parent = parent.to_string();
        let mut messages = Vec::new();
        let mut cursor = String::new();
        loop {
            let page: Replies = self
                .call(
                    "conversations.replies",
                    &[
                        ("channel", conversation.as_str()),
                        ("ts", &parent),
                        ("limit", "200"),
                        ("cursor", &cursor),
                    ],
                )
                .await?;
            let next = page.next_cursor().to_owned();
            messages.extend(page.into_messages());
            if next.is_empty() || messages.len() > MAX_REPLIES {
                return Ok(messages);
            }
            cursor = next;
        }
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

    /// Moves the user's Read Marker in the Thread under `parent` to `up_to`.
    pub(crate) async fn mark_thread(
        &self,
        conversation: &ConversationId,
        parent: Ts,
        up_to: Ts,
    ) -> Result<(), Error> {
        let (parent, up_to) = (parent.to_string(), up_to.to_string());
        let params = [
            ("channel", conversation.as_str()),
            ("thread_ts", parent.as_str()),
            ("ts", up_to.as_str()),
            ("read", "1"),
        ];
        let _: serde::de::IgnoredAny = self.call("subscriptions.thread.mark", &params).await?;
        Ok(())
    }

    /// Up to [`PEOPLE_FOUND`] people in `workspace` whose names match `query`, best first. Deactivated accounts
    /// aren't among them.
    pub(crate) async fn find_people(
        &self,
        workspace: &WorkspaceId,
        query: &str,
    ) -> Result<Vec<Person>, Error> {
        // As the web client asks, less what it would show beyond names.
        let body = json!({
            "query": query,
            "count": PEOPLE_FOUND,
            "fuzz": 1,
            "uax29_tokenizer": false,
            "filter": "NOT deactivated",
        });
        let found: PeopleSearch = self.call_edge(workspace, "users/search", &body).await?;
        Ok(found.into_people())
    }

    pub(crate) async fn emoji(&self) -> Result<EmojiList, Error> {
        let listed: ListedEmoji = self
            .call("emoji.list", &[("include_categories", "true")])
            .await?;
        Ok(listed.into_list())
    }

    /// How many times the user used each emoji, by name, as Slack's clients count it.
    pub(crate) async fn emoji_use(&self) -> Result<HashMap<String, u32>, Error> {
        let prefs: Prefs = self.call("users.prefs.get", &[]).await?;
        prefs
            .into_emoji_use()
            .ok_or_else(|| Error::refused("users.prefs.get", "unreadable_emoji_use"))
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
