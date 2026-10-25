/// Whether someone is using Slack right now, as Slack's green dot by their name says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presence {
    /// Using Slack, or was a short while ago.
    Active,
    /// Not using Slack lately, or set themselves away.
    Away,
}
