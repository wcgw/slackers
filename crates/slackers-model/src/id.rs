use std::fmt;
use std::str::FromStr;

macro_rules! slack_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Wraps an id as Slack spells it, such as `C024BE91L`.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The id as Slack spells it.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

slack_id!(
    /// A Workspace: Slack's team id (`T…`), or the enterprise id (`E…`) of an Enterprise Grid org.
    WorkspaceId
);
slack_id!(
    /// A Conversation, unique within its Workspace: a Channel (`C…`, or `G…` for older private ones) or a DM
    /// (`D…`).
    ConversationId
);
slack_id!(
    /// A person or bot, unique within its Workspace (`U…`, or `W…` on Enterprise Grid).
    UserId
);
slack_id!(
    /// A bot, which an app or integration posts through (`B…`), unique within its Workspace.
    BotId
);

/// When a Message was posted, which is also its id within its Conversation: microseconds since the Unix epoch.
///
/// Slack spells it as seconds with six decimals (`"1700000000.123456"`). Parsing and [`Display`](fmt::Display)
/// keep that exact, so a `Ts` round-trips. The zero `Ts` ([`Ts::default`]) precedes every real Message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ts(u64);

impl Ts {
    /// The `Ts` `micros` microseconds after the Unix epoch.
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// Microseconds since the Unix epoch.
    pub const fn as_micros(self) -> u64 {
        self.0
    }
}

impl FromStr for Ts {
    type Err = ParseTsError;

    /// Parses Slack's spelling: whole seconds, optionally followed by `.` and up to six digits.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseTsError(text.to_owned());
        let (seconds, fraction) = text.split_once('.').unwrap_or((text, ""));
        let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if seconds.is_empty() || fraction.len() > 6 || !all_digits(seconds) || !all_digits(fraction)
        {
            return Err(invalid());
        }
        let seconds: u64 = seconds.parse().map_err(|_| invalid())?;
        // "5" means 500000 µs: pad the fraction to six digits.
        let micros = fraction
            .bytes()
            .chain(std::iter::repeat(b'0'))
            .take(6)
            .fold(0, |micros, digit| micros * 10 + u64::from(digit - b'0'));
        seconds
            .checked_mul(1_000_000)
            .and_then(|micros_of_seconds| micros_of_seconds.checked_add(micros))
            .map(Self)
            .ok_or_else(invalid)
    }
}

impl fmt::Display for Ts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:06}", self.0 / 1_000_000, self.0 % 1_000_000)
    }
}

/// Text that isn't a Slack timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseTsError(String);

impl fmt::Display for ParseTsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid slack timestamp {:?}", self.0)
    }
}

impl std::error::Error for ParseTsError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_round_trips_slack_spelling() {
        let ts: Ts = "1700000000.123456".parse().unwrap();
        assert_eq!(ts.as_micros(), 1_700_000_000_123_456);
        assert_eq!(ts.to_string(), "1700000000.123456");
    }

    #[test]
    fn ts_pads_short_fractions_and_accepts_whole_seconds() {
        assert_eq!("12.5".parse::<Ts>().unwrap(), Ts::from_micros(12_500_000));
        assert_eq!("12".parse::<Ts>().unwrap(), Ts::from_micros(12_000_000));
        assert_eq!(Ts::from_micros(12_000_001).to_string(), "12.000001");
    }

    #[test]
    fn ts_orders_by_time_not_by_text() {
        let earlier: Ts = "999999999.999999".parse().unwrap();
        let later: Ts = "1000000000.000000".parse().unwrap();
        assert!(earlier < later);
    }

    #[test]
    fn ts_rejects_what_slack_never_sends() {
        for bad in [
            "",
            ".5",
            "1.1234567",
            "1e9",
            "-1.0",
            "1.2.3",
            "18446744073709.551616",
        ] {
            assert!(bad.parse::<Ts>().is_err(), "{bad:?} parsed");
        }
    }
}
