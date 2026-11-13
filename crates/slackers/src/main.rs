//! slackers: a terminal Slack client that groups Conversations by subject across Workspaces.
//!
//! ```sh
//! slackers [WORKSPACE…]
//! ```
//!
//! signs in to each `WORKSPACE` with the credentials stored in the keyring under that name (see [`credentials`]),
//! or to the `workspaces` the config file lists (see [`config`]), and shows the Virtual Workspaces it defines. It runs
//! in wcgw-kit, which gives it Neovim's keys, windows and commands (see [`app`]); `:q` quits. Logs go to
//! `$XDG_STATE_HOME/slackers/slackers.log`; `RUST_LOG` sets their level.

mod activity;
mod app;
mod config;
mod credentials;
mod draft;
mod emoji;
mod hl;
mod icons;
mod layout;
mod markdown;
mod runtime;
mod search;
mod sidebar;
mod typing;

use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context as _, Result, ensure};
use tracing_subscriber::EnvFilter;

use crate::config::Config;

fn main() -> Result<()> {
    let config_path = config::path()?;
    let config = Config::load(&config_path)?;
    let named: Vec<String> = std::env::args().skip(1).collect();
    let labels = if named.is_empty() {
        config.workspaces
    } else {
        named
    };
    ensure!(
        !labels.is_empty(),
        "usage: slackers WORKSPACE…, or list workspaces = [\"WORKSPACE\", …] in {}",
        config_path.display()
    );
    start_logging()?;
    // Before the runtime starts: reading the keyring blocks.
    let credentials = labels
        .iter()
        .map(|label| credentials::load(label))
        .collect();
    // The workers mostly wait on Slack, and the UI runs on this thread: tokio's default of one worker per core is far
    // more than they need.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    tokio.block_on(runtime::run(
        labels,
        config.virtual_workspaces,
        credentials,
        config_path,
    ))?;
    Ok(())
}

/// Sends logs to `$XDG_STATE_HOME/slackers/slackers.log`: the terminal belongs to the UI.
fn start_logging() -> Result<()> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("neither XDG_STATE_HOME nor HOME is set")?;
    let directory = state.join("slackers");
    fs::create_dir_all(&directory).with_context(|| format!("creating {}", directory.display()))?;
    let path = directory.join("slackers.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    Ok(())
}
