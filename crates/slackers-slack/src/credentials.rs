use std::fmt;

/// What signs a [`Session`](crate::Session) in: a Workspace's `xoxc-` token and the browser session's cookies.
///
/// They grant full access as the user, so [`Debug`] doesn't show them, and nothing in this crate logs them;
/// [`Credentials::reveal`], [`Credentials::d`] and [`Credentials::d_s`] are the only ways out.
#[derive(Clone)]
pub struct Credentials {
    token: String,
    /// The browser session's `d` cookie.
    d: String,
    /// The browser session's `d-s` cookie, when it has one.
    d_s: Option<String>,
    /// The `Cookie` header value, which `d` and `d_s` make up.
    cookies: String,
}

impl Credentials {
    /// Credentials from a Workspace's `token` and the browser's `d` cookie, plus its `d-s` cookie when the browser
    /// has one. Cookie values are used as copied from the browser, still percent-encoded.
    pub fn new(token: String, d: &str, d_s: Option<&str>) -> Self {
        let cookies = match d_s {
            Some(d_s) => format!("d={d}; d-s={d_s}"),
            None => format!("d={d}"),
        };
        Self {
            token,
            d: d.to_owned(),
            d_s: d_s.map(str::to_owned),
            cookies,
        }
    }

    /// The browser session's `d` cookie, in the clear: for the caller to log when the user asked for it, and for
    /// nothing else.
    pub fn d(&self) -> &str {
        &self.d
    }

    /// The browser session's `d-s` cookie, in the clear, when it has one; as [`Credentials::d`].
    pub fn d_s(&self) -> Option<&str> {
        self.d_s.as_deref()
    }

    /// The token and the cookies in the clear, as they go to Slack. For telling the user what signing in used when
    /// they asked: keep it to the screen in front of them, and the token out of the log.
    pub fn reveal(&self) -> String {
        format!("token={}; {}", self.token, self.cookies)
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn cookies(&self) -> &str {
        &self.cookies
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_secrets() {
        let credentials = Credentials::new("xoxc-secret".into(), "xoxd-secret", Some("ds"));
        assert_eq!(format!("{credentials:?}"), "Credentials { .. }");
        assert_eq!(credentials.cookies(), "d=xoxd-secret; d-s=ds");
    }

    #[test]
    fn revealing_them_shows_the_token_and_the_cookies() {
        let credentials = Credentials::new("xoxc-secret".into(), "xoxd-secret", Some("ds"));
        assert_eq!(
            credentials.reveal(),
            "token=xoxc-secret; d=xoxd-secret; d-s=ds"
        );
        let without = Credentials::new("xoxc-secret".into(), "xoxd-secret", None);
        assert_eq!(without.reveal(), "token=xoxc-secret; d=xoxd-secret");
    }
}
