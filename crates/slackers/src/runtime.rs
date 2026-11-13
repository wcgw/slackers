//! slackers in the kit's runtime (its D2): the terminal, the event loop and the kit's effects are the kit's;
//! slackers brings its Workspaces' Sessions, the pictures they serve, and its config file.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use slackers_slack::{Credentials, Downloader, Session};
use wcgw_kit::runtime::{self, Bytes, Inbox, Perform};

use crate::app::{Io, Msg, Slackers, WorkspaceKey};
use crate::config::{self, VirtualWorkspace};

/// Runs slackers until it quits, signing in to the Workspaces with these `labels` with the `credentials` at the
/// same place, listing `virtual_workspaces`, and saving changes to them in the config file at `config`.
///
/// # Errors
///
/// If the terminal can't be set up, read from or drawn to.
pub(crate) async fn run(
    labels: Vec<String>,
    virtual_workspaces: Vec<VirtualWorkspace>,
    credentials: Vec<anyhow::Result<Credentials>>,
    config: PathBuf,
) -> io::Result<()> {
    let (inbox, mailbox) = runtime::channel();
    let (app, kit) = Slackers::session(labels, virtual_workspaces);
    let mut performer = Performer {
        sessions: HashMap::new(),
        downloaders: HashMap::new(),
        config,
        inbox: inbox.clone(),
    };
    for (key, credentials) in app.workspace_keys().zip(credentials) {
        match credentials {
            Ok(credentials) => {
                match Downloader::new(credentials.clone()) {
                    Ok(downloader) => {
                        performer.downloaders.insert(key, downloader);
                    }
                    Err(error) => tracing::warn!(%error, "can't download pictures"),
                }
                let inbox = inbox.clone();
                // Runs on the Session's tasks. Sending fails only once the loop has ended.
                let session = Session::start(credentials, move |update| {
                    inbox.send(Msg::Slack(key, update));
                });
                performer.sessions.insert(key, session);
            }
            Err(error) => {
                inbox.send(Msg::CantSignIn {
                    workspace: key,
                    reason: format!("{error:#}"),
                });
            }
        }
    }
    runtime::run("slackers", app, kit, performer, (inbox, mailbox)).await
}

/// Carries out slackers' effects: asks the Workspaces' Sessions, fetches pictures, saves the config.
struct Performer {
    /// Each Workspace's Session, for those whose credentials were found.
    sessions: HashMap<WorkspaceKey, Session>,
    /// What fetches each Workspace's pictures, with its credentials.
    downloaders: HashMap<WorkspaceKey, Downloader>,
    /// Where the config file is.
    config: PathBuf,
    /// Where saving reports a failure.
    inbox: Inbox<Msg>,
}

impl Perform<Slackers> for Performer {
    fn perform(&mut self, effect: Io) {
        match effect {
            Io::Slack(workspace, request) => {
                if let Some(session) = self.sessions.get(&workspace) {
                    session.request(request);
                }
            }
            Io::SaveVirtualWorkspaces(virtual_workspaces) => {
                // A small file, rarely written: write it right away, on this thread, so that saves can't overtake
                // one another.
                let saved = tokio::task::block_in_place(|| {
                    config::save_virtual_workspaces(&self.config, &virtual_workspaces)
                });
                if let Err(error) = saved {
                    tracing::warn!(error = format!("{error:#}"), "couldn't save the config");
                    self.inbox.send(Msg::SaveFailed(format!("{error:#}")));
                }
            }
        }
    }

    /// The picture at `source`, through the Workspace it names: Slack serves a Workspace's files to its members only.
    fn picture(&mut self, source: &str) -> Bytes {
        let found = crate::app::picture_of(source)
            .and_then(|(key, url)| Some((self.downloaders.get(&key)?.clone(), url.to_owned())));
        Box::pin(async move {
            let (downloader, url) = found.ok_or("not signed in where it is")?;
            downloader
                .download(&url)
                .await
                .map_err(|error| error.to_string())
        })
    }
}
