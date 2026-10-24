/// Why talking to Slack failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Slack no longer accepts the credentials, typically because the browser session they came from ended or
    /// its `d` cookie was replaced. Copy fresh ones from the browser.
    #[error("slack no longer accepts these credentials; copy a fresh d cookie from the browser")]
    SignedOut,
    /// Slack answered, but refused: `code` is its error code, such as `channel_not_found`.
    #[error("slack refused {method}: {code}")]
    Refused {
        /// The Web API method, or `download`.
        method: &'static str,
        /// Slack's error code.
        code: String,
    },
    /// Slack couldn't be reached, or the connection broke.
    #[error("couldn't reach slack")]
    Network(#[from] reqwest::Error),
    /// Slack answered with something this version doesn't understand.
    #[error("unexpected reply from slack to {method}")]
    Unexpected {
        /// The Web API method.
        method: &'static str,
        /// What didn't parse.
        #[source]
        source: serde_json::Error,
    },
}

impl Error {
    /// The error for Slack's `ok: false` reply with error `code` to `method`.
    pub(crate) fn refused(method: &'static str, code: &str) -> Self {
        match code {
            "invalid_auth" | "not_authed" | "account_inactive" | "token_revoked"
            | "token_expired" => Self::SignedOut,
            _ => Self::Refused {
                method,
                code: code.to_owned(),
            },
        }
    }
}
