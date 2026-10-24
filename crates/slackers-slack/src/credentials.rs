use std::fmt;

/// What signs a [`Session`](crate::Session) in: a Workspace's `xoxc-` token and the browser session's cookies.
///
/// They grant full access as the user, so [`Debug`] doesn't show them, and nothing in this crate logs them.
#[derive(Clone)]
pub struct Credentials {
    token: String,
    /// The `Cookie` header value.
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
        Self { token, cookies }
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
}
