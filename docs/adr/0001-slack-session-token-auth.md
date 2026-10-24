---
status: accepted
---

# Sign in with Slack session tokens, and talk to Slack the way its web client does

slackers signs in to each Workspace with the web client's session credentials: an `xoxc-` token per Workspace plus the
browser's `d` cookie (and `d-s`, when present). It does not install a Slack app. Since 2025-05-29, Slack limits
distributed apps that aren't approved for the Marketplace to one request per minute and 15 messages per call on
`conversations.history` and `conversations.replies`
([changelog](https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps)). That rules out
one app shared across Workspaces. An internal app in each Workspace would need an admin's approval in every Workspace,
which we can't count on.

Because Red Hat's Enterprise Grid restricts documented methods for third-party tools, slackers uses the methods the
web client itself depends on, not the documented equivalents:

| Need | Uses | Refused on Red Hat's Grid (`enterprise_is_restricted`) |
|---|---|---|
| Conversations, names, sidebar preferences | `client.userBoot` | `users.conversations`, `client.boot` |
| Unread state at startup | `client.counts` | `users.counts` |
| Followed Threads | `subscriptions.thread.getView` | |
| Live events | the web client's socket: `wss://wss-primary.slack.com/?token=…&gateway_server=<id>-1&slack_client=desktop&batch_presence_aware=1`, where `<id>` is the team id, or the enterprise id for an org-level token (as wee-slack does) | `rtm.connect` |

`conversations.history`, `.replies` and `.info`, `users.info`, `emoji.list` and file downloads work as documented.

A spike on 2026-10-24 confirmed all of this against Red Hat (Enterprise Grid, org-level token) and CNCF. Short bursts
of history calls weren't rate limited.

## Consequences

- Slack doesn't endorse this, and the web-client methods are undocumented. They can change without notice. Keep all
  knowledge of them inside the Slack adapter, so that switching to a documented method or an app token stays a local
  change. Don't "fix" them back to the documented methods: on Red Hat's Grid those are refused.
- The credentials grant full access as the user. Treat them as a password: never log them, and never store them in
  plain config. They live in the Secret Service keyring.
- The credentials belong to a browser session. Signing in to another Workspace in that browser replaces its `d` cookie
  and invalidates the stored one; logging out does too. The tokens survive, so refreshing the cookie is enough.
  slackers must detect `invalid_auth` and ask for fresh credentials instead of failing. Later, slackers could get a
  session of its own that the browser can't disturb.
- Downloading a private file without valid credentials returns HTTP 200 with an HTML login page, not an error. Check
  the content type.
