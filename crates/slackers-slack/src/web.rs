//! Where things are in Slack's web client, for opening them in a browser.

use slackers_model::{ConversationId, Ts};

/// Builds links to a Workspace's Messages, as the web client shares them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permalinks {
    /// Where the Workspace is on the web, ending in `/`.
    base: String,
}

impl Permalinks {
    /// Links under `base`, where the Workspace is on the web, such as `https://example.slack.com/`.
    pub fn new(base: impl Into<String>) -> Self {
        let mut base = base.into();
        if !base.ends_with('/') {
            base.push('/');
        }
        Self { base }
    }

    /// The link to the Message posted at `ts` in `conversation`. `thread` is the parent of the Thread the Message
    /// is in or starts, if any, which the link then opens.
    pub fn message(&self, conversation: &ConversationId, ts: Ts, thread: Option<Ts>) -> String {
        // Slack spells the timestamp without its dot here.
        let ts = ts.to_string().replace('.', "");
        let mut link = format!("{}archives/{conversation}/p{ts}", self.base);
        if let Some(parent) = thread {
            link.push_str(&format!("?thread_ts={parent}&cid={conversation}"));
        }
        link
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_links_to_its_conversation_and_to_its_thread_if_any() {
        let links = Permalinks::new("https://example.slack.com");
        let (channel, ts) = (
            ConversationId::new("C1"),
            "1700000000.012345".parse().unwrap(),
        );
        assert_eq!(
            links.message(&channel, ts, None),
            "https://example.slack.com/archives/C1/p1700000000012345"
        );
        let parent = "1699999999.000100".parse().unwrap();
        assert_eq!(
            links.message(&channel, ts, Some(parent)),
            "https://example.slack.com/archives/C1/p1700000000012345?thread_ts=1699999999.000100&cid=C1"
        );
    }
}
