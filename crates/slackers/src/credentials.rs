//! Session credentials, kept in the Secret Service keyring as `secret-tool store` puts them (see
//! `docs/adr/0001-slack-session-token-auth.md`):
//!
//! ```sh
//! secret-tool store --label='slackers d cookie'      service slackers key d-cookie
//! secret-tool store --label='slackers d-s cookie'    service slackers key d-s-cookie   # only if the browser has one
//! secret-tool store --label='slackers WORKSPACE token' service slackers key token workspace WORKSPACE
//! ```

use std::process::Command;

use anyhow::{Context as _, Result, bail};
use slackers_slack::Credentials;

/// `workspace`'s credentials from the keyring. Blocks while `secret-tool` runs, which may prompt to unlock the
/// keyring.
///
/// # Errors
///
/// If `secret-tool` can't run, or the token or the `d` cookie isn't stored.
pub(crate) fn load(workspace: &str) -> Result<Credentials> {
    let token = lookup(&["key", "token", "workspace", workspace])?.with_context(|| {
        format!("no token for workspace {workspace:?} in the keyring; see `secret-tool store`")
    })?;
    let d = lookup(&["key", "d-cookie"])?
        .context("no d cookie in the keyring; see `secret-tool store`")?;
    let d_s = lookup(&["key", "d-s-cookie"])?;
    Ok(Credentials::new(token, &d, d_s.as_deref()))
}

/// The secret stored under `service slackers` and `attributes`; `None` when there is none.
fn lookup(attributes: &[&str]) -> Result<Option<String>> {
    let output = Command::new("secret-tool")
        .args(["lookup", "service", "slackers"])
        .args(attributes)
        .output()
        .context("couldn't run secret-tool (from libsecret)")?;
    // secret-tool exits with 1, saying nothing, when there's no such secret.
    if !output.status.success() {
        if output.stderr.is_empty() {
            return Ok(None);
        }
        bail!(
            "secret-tool failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let secret = String::from_utf8(output.stdout).context("a stored secret isn't UTF-8")?;
    Ok(Some(secret.trim_end_matches('\n').to_owned()))
}
