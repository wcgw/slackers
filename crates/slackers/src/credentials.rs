//! Session credentials, kept in the system keyring (see `docs/adr/0001-slack-session-token-auth.md`, and the README
//! for how to store them): the Secret Service on Linux, as `secret-tool store` puts them, and the login Keychain on
//! macOS, as `security add-generic-password` does.

use std::process::{Command, Output};

use anyhow::{Context as _, Result, bail};
use slackers_slack::Credentials;

/// `workspace`'s credentials from the keyring. Blocks while the keyring's tool runs, which may prompt to unlock the
/// keyring.
///
/// # Errors
///
/// If the keyring's tool can't run, or the token or the `d` cookie isn't stored.
pub(crate) fn load(workspace: &str) -> Result<Credentials> {
    let keyring = Keyring::HERE;
    let missing = |secret| {
        format!(
            "no {} in the keyring; store it with `{}`",
            Secret::describe(secret),
            keyring.store(secret)
        )
    };
    let token = keyring
        .read(Secret::Token(workspace))?
        .with_context(|| missing(Secret::Token(workspace)))?;
    let d = keyring
        .read(Secret::DCookie)?
        .with_context(|| missing(Secret::DCookie))?;
    let d_s = keyring.read(Secret::DsCookie)?;
    Ok(Credentials::new(token, &d, d_s.as_deref()))
}

/// A secret slackers keeps in the keyring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Secret<'a> {
    /// A Workspace's `xoxc-` token, by the label the config gives the Workspace.
    Token(&'a str),
    /// The browser session's `d` cookie, which every Workspace shares.
    DCookie,
    /// The browser session's `d-s` cookie, when it has one.
    DsCookie,
}

impl Secret<'_> {
    fn describe(self) -> String {
        match self {
            Secret::Token(workspace) => format!("token for workspace {workspace:?}"),
            Secret::DCookie => "d cookie".to_owned(),
            Secret::DsCookie => "d-s cookie".to_owned(),
        }
    }
}

/// Where secrets are kept, and how to read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keyring {
    /// The Secret Service, through `secret-tool` from libsecret: Linux.
    SecretService,
    /// The login Keychain, through `security`: macOS.
    Keychain,
}

impl Keyring {
    /// This platform's.
    const HERE: Self = if cfg!(target_os = "macos") {
        Self::Keychain
    } else {
        Self::SecretService
    };

    /// The secret stored as `secret`; `None` when there is none.
    fn read(self, secret: Secret<'_>) -> Result<Option<String>> {
        let program = self.program();
        let output = self
            .lookup(secret)
            .output()
            .with_context(|| format!("couldn't run {program}"))?;
        if !output.status.success() {
            if self.missing(&output) {
                return Ok(None);
            }
            bail!(
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let secret = String::from_utf8(output.stdout).context("a stored secret isn't UTF-8")?;
        Ok(Some(secret.trim_end_matches('\n').to_owned()))
    }

    fn program(self) -> &'static str {
        match self {
            Keyring::SecretService => "secret-tool",
            Keyring::Keychain => "security",
        }
    }

    /// The command that prints `secret`.
    fn lookup(self, secret: Secret<'_>) -> Command {
        let mut command = Command::new(self.program());
        match self {
            Keyring::SecretService => {
                command.args(["lookup", "service", "slackers"]);
                command.args(attributes(secret));
            }
            Keyring::Keychain => {
                let account = account(secret);
                command.args([
                    "find-generic-password",
                    "-s",
                    "slackers",
                    "-a",
                    &account,
                    "-w",
                ]);
            }
        }
        command
    }

    /// Whether `output`, from [`Keyring::lookup`], says only that there's no such secret.
    fn missing(self, output: &Output) -> bool {
        match self {
            // secret-tool exits with 1, saying nothing.
            Keyring::SecretService => output.stderr.is_empty(),
            // security exits with 44, errSecItemNotFound.
            Keyring::Keychain => output.status.code() == Some(44),
        }
    }

    /// The command that stores `secret`, prompting for it.
    fn store(self, secret: Secret<'_>) -> String {
        match self {
            Keyring::SecretService => format!(
                "secret-tool store --label='slackers {}' service slackers {}",
                secret.describe(),
                attributes(secret).join(" ")
            ),
            Keyring::Keychain => format!(
                "security add-generic-password -U -s slackers -a {} -w",
                account(secret)
            ),
        }
    }
}

/// `secret`'s attributes in the Secret Service, besides `service slackers`.
fn attributes(secret: Secret<'_>) -> Vec<&str> {
    match secret {
        Secret::Token(workspace) => vec!["key", "token", "workspace", workspace],
        Secret::DCookie => vec!["key", "d-cookie"],
        Secret::DsCookie => vec!["key", "d-s-cookie"],
    }
}

/// `secret`'s account in the Keychain, under the service `slackers`.
fn account(secret: Secret<'_>) -> String {
    match secret {
        Secret::Token(workspace) => format!("token:{workspace}"),
        Secret::DCookie => "d-cookie".to_owned(),
        Secret::DsCookie => "d-s-cookie".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;

    use super::*;

    fn args(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn output(code: i32, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn the_secret_service_keeps_them_under_slackers_attributes() {
        let keyring = Keyring::SecretService;
        assert_eq!(
            args(&keyring.lookup(Secret::Token("cncf"))),
            [
                "secret-tool",
                "lookup",
                "service",
                "slackers",
                "key",
                "token",
                "workspace",
                "cncf"
            ]
        );
        assert_eq!(
            args(&keyring.lookup(Secret::DsCookie)),
            [
                "secret-tool",
                "lookup",
                "service",
                "slackers",
                "key",
                "d-s-cookie"
            ]
        );
        assert_eq!(
            keyring.store(Secret::DCookie),
            "secret-tool store --label='slackers d cookie' service slackers key d-cookie"
        );
        assert!(keyring.missing(&output(1, "")));
        assert!(!keyring.missing(&output(1, "the keyring is locked")));
    }

    #[test]
    fn the_keychain_keeps_them_as_generic_passwords_of_slackers() {
        let keyring = Keyring::Keychain;
        assert_eq!(
            args(&keyring.lookup(Secret::Token("cncf"))),
            [
                "security",
                "find-generic-password",
                "-s",
                "slackers",
                "-a",
                "token:cncf",
                "-w"
            ]
        );
        assert_eq!(
            args(&keyring.lookup(Secret::DCookie)),
            [
                "security",
                "find-generic-password",
                "-s",
                "slackers",
                "-a",
                "d-cookie",
                "-w"
            ]
        );
        assert_eq!(
            keyring.store(Secret::DsCookie),
            "security add-generic-password -U -s slackers -a d-s-cookie -w"
        );
        assert!(keyring.missing(&output(44, "The specified item could not be found")));
        assert!(!keyring.missing(&output(51, "user interaction is not allowed")));
    }

    #[test]
    fn this_platform_uses_its_own() {
        let expected = if cfg!(target_os = "macos") {
            Keyring::Keychain
        } else {
            Keyring::SecretService
        };
        assert_eq!(Keyring::HERE, expected);
    }
}
