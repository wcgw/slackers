use std::sync::Arc;

/// Why talking to Slack failed.
///
/// It can be cloned and compared, so that the [`Update`](crate::Update)s carrying it can be: what went wrong
/// underneath is shared, and two errors are the same when they say the same.
#[derive(Clone, Debug, thiserror::Error)]
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
    Network(#[source] Arc<reqwest::Error>),
    /// Slack answered with something this version doesn't understand.
    #[error("unexpected reply from slack to {method}")]
    Unexpected {
        /// The Web API method.
        method: &'static str,
        /// What didn't parse.
        #[source]
        source: Arc<serde_json::Error>,
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

    /// The error for a reply to `method` that didn't parse.
    pub(crate) fn unexpected(method: &'static str, source: serde_json::Error) -> Self {
        Self::Unexpected {
            method,
            source: Arc::new(source),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Network(Arc::new(error))
    }
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::SignedOut, Self::SignedOut) => true,
            (
                Self::Refused { method, code },
                Self::Refused {
                    method: other_method,
                    code: other_code,
                },
            ) => method == other_method && code == other_code,
            (Self::Network(error), Self::Network(other)) => {
                Arc::ptr_eq(error, other) || error.to_string() == other.to_string()
            }
            (
                Self::Unexpected { method, source },
                Self::Unexpected {
                    method: other_method,
                    source: other,
                },
            ) => method == other_method && source.to_string() == other.to_string(),
            _ => false,
        }
    }
}

impl Eq for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_the_same_when_they_say_the_same() {
        let parse = |text: &str| serde_json::from_str::<u32>(text).unwrap_err();
        assert_eq!(Error::SignedOut, Error::SignedOut.clone());
        assert_eq!(
            Error::unexpected("auth.test", parse("x")),
            Error::unexpected("auth.test", parse("x"))
        );
        assert_ne!(
            Error::unexpected("auth.test", parse("x")),
            Error::unexpected("users.info", parse("x"))
        );
        assert_ne!(Error::SignedOut, Error::refused("chat.postMessage", "nope"));
    }
}
