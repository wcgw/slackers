//! The live socket, as the web client opens it. `rtm.connect` is refused on Red Hat's Enterprise Grid; this isn't
//! (see `docs/adr/0001-slack-session-token-auth.md`).

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use slackers_model::WorkspaceId;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::Credentials;
use crate::session::{Connection, Heard, Notify, Update};
use crate::wire::{SocketMessage, socket_message};

/// How often to ping Slack, so that it, and anything in between, keeps the connection open.
const PING_EVERY: Duration = Duration::from_secs(30);

/// The longest wait between reconnection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Follows the Workspace's live events forever, reconnecting with backoff whenever the connection drops. What it
/// hears goes to `heard`; how the connection stands, to `notify`. Runs until its task is aborted.
pub(crate) async fn follow(
    credentials: Credentials,
    gateway: WorkspaceId,
    heard: mpsc::UnboundedSender<Heard>,
    notify: Notify,
) {
    let Ok(cookies) = HeaderValue::from_str(credentials.cookies()) else {
        tracing::error!(
            "the session cookies contain characters a header can't; not following live events"
        );
        return;
    };
    let mut reconnect_url = None;
    let mut attempt: u32 = 0;
    loop {
        let url = reconnect_url.take().unwrap_or_else(|| {
            format!(
                "wss://wss-primary.slack.com/?token={}&gateway_server={gateway}-1&slack_client=desktop&batch_presence_aware=1",
                credentials.token()
            )
        });
        match connect_and_read(
            &url,
            &cookies,
            &heard,
            &notify,
            &mut reconnect_url,
            &mut attempt,
        )
        .await
        {
            Ok(()) => tracing::info!("slack closed the live connection"),
            // The error never includes the URL, which carries the token.
            Err(error) => tracing::warn!(%error, "live connection to slack failed"),
        }
        attempt += 1;
        notify(Update::Connection(Connection::Reconnecting { attempt }));
        let backoff = Duration::from_secs(1u64 << attempt.min(6)).min(MAX_BACKOFF);
        tokio::time::sleep(backoff).await;
    }
}

/// Connects to `url` and forwards events until the connection ends. Saves where to reconnect in `reconnect_url`,
/// and resets `attempt` once Slack says hello.
async fn connect_and_read(
    url: &str,
    cookies: &HeaderValue,
    heard: &mpsc::UnboundedSender<Heard>,
    notify: &Notify,
    reconnect_url: &mut Option<String>,
    attempt: &mut u32,
) -> Result<(), tungstenite::Error> {
    let mut request = url.into_client_request()?;
    request.headers_mut().insert("Cookie", cookies.clone());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await; // The first tick is immediate.
    let mut ping_id: u64 = 0;
    loop {
        tokio::select! {
            frame = socket.next() => match frame {
                None | Some(Ok(Message::Close(_))) => return Ok(()),
                Some(Err(error)) => return Err(error),
                Some(Ok(Message::Text(text))) => match socket_message(&text) {
                    SocketMessage::Hello => {
                        *attempt = 0;
                        notify(Update::Connection(Connection::Live));
                    }
                    SocketMessage::ReconnectUrl(url) => *reconnect_url = Some(url),
                    // Sending fails only once the Session is stopping.
                    SocketMessage::Live(event) => {
                        let _ = heard.send(Heard::Event(event));
                    }
                    SocketMessage::Appeared(conversation) => {
                        let _ = heard.send(Heard::Appeared(conversation));
                    }
                    SocketMessage::Ignored => {}
                },
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                ping_id += 1;
                socket.send(Message::text(format!(r#"{{"type":"ping","id":{ping_id}}}"#))).await?;
            }
        }
    }
}
