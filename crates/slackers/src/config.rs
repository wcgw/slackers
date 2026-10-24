//! The user's settings, in `$XDG_CONFIG_HOME/slackers/config.toml`:
//!
//! ```toml
//! # Workspaces to sign in to, by their name in the keyring, in sidebar order.
//! workspaces = ["redhat", "cncf"]
//!
//! [[virtual_workspace]]
//! name = "Kuadrant"
//! channels = ["redhat#kuadrant", "cncf#kuadrant-dev"]
//! ```
//!
//! slackers rewrites the file when Virtual Workspaces change, keeping its comments and layout.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value, value};

/// What the config file says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Config {
    /// Workspaces to sign in to, by their name in the keyring, in sidebar order.
    pub(crate) workspaces: Vec<String>,
    pub(crate) virtual_workspaces: Vec<VirtualWorkspace>,
}

/// A Virtual Workspace, as the config defines it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VirtualWorkspace {
    pub(crate) name: String,
    /// Its Channels, in sidebar order.
    pub(crate) channels: Vec<ChannelRef>,
}

/// A Channel, by its Workspace's name in the config and its own name, spelled `redhat#kuadrant`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ChannelRef {
    pub(crate) workspace: String,
    pub(crate) channel: String,
}

impl ChannelRef {
    /// Parses `WORKSPACE#CHANNEL`; `None` unless both parts are there.
    fn parse(text: &str) -> Option<Self> {
        let (workspace, channel) = text.split_once('#')?;
        (!workspace.is_empty() && !channel.is_empty()).then(|| Self {
            workspace: workspace.to_owned(),
            channel: channel.to_owned(),
        })
    }
}

impl fmt::Display for ChannelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.workspace, self.channel)
    }
}

/// Where the config file is: under `$XDG_CONFIG_HOME`, else `~/.config`.
///
/// # Errors
///
/// If neither `XDG_CONFIG_HOME` nor `HOME` is set.
pub(crate) fn path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(base.join("slackers").join("config.toml"))
}

impl Config {
    /// Reads the config at `path`. A missing file is an empty config.
    ///
    /// # Errors
    ///
    /// If the file can't be read or doesn't say what slackers expects.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn parse(text: &str) -> Result<Self> {
        let document: DocumentMut = text.parse()?;
        let strings = |item: &Item, what: &str| -> Result<Vec<String>> {
            let Some(array) = item.as_array() else {
                bail!("{what} must be a list of strings")
            };
            array
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .with_context(|| format!("{what} must hold strings"))
                })
                .collect()
        };
        let workspaces = match document.get("workspaces") {
            Some(item) => strings(item, "workspaces")?,
            None => Vec::new(),
        };
        let mut virtual_workspaces = Vec::new();
        if let Some(item) = document.get("virtual_workspace") {
            let tables = item
                .as_array_of_tables()
                .context("virtual_workspace must be [[virtual_workspace]] tables")?;
            for table in tables {
                let name = table
                    .get("name")
                    .and_then(Item::as_str)
                    .context("a virtual_workspace has no name")?;
                let channels = match table.get("channels") {
                    Some(item) => strings(item, "channels")?
                        .iter()
                        .map(|text| {
                            ChannelRef::parse(text).with_context(|| {
                                format!("virtual workspace {name:?}: channel {text:?} isn't WORKSPACE#CHANNEL")
                            })
                        })
                        .collect::<Result<_>>()?,
                    None => Vec::new(),
                };
                virtual_workspaces.push(VirtualWorkspace {
                    name: name.to_owned(),
                    channels,
                });
            }
        }
        Ok(Self {
            workspaces,
            virtual_workspaces,
        })
    }
}

/// Rewrites the Virtual Workspaces in the config file at `path` (creating it if need be), keeping everything else.
///
/// # Errors
///
/// If the file can't be read, parsed or written.
pub(crate) fn save_virtual_workspaces(
    path: &Path,
    virtual_workspaces: &[VirtualWorkspace],
) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let updated = with_virtual_workspaces(&text, virtual_workspaces)?;
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }
    // Write beside it, then move it over, so that a crash can't leave half a file.
    let staged = path.with_extension("toml.new");
    fs::write(&staged, updated).with_context(|| format!("writing {}", staged.display()))?;
    fs::rename(&staged, path).with_context(|| format!("replacing {}", path.display()))
}

/// `text` with its Virtual Workspaces replaced by `wanted`. Each one that stays keeps its table, comments and all;
/// those no longer wanted go, and new ones are added at the end.
fn with_virtual_workspaces(text: &str, wanted: &[VirtualWorkspace]) -> Result<String> {
    let mut document: DocumentMut = text.parse()?;
    let name_of = |table: &Table| table.get("name").and_then(Item::as_str).map(str::to_owned);
    if wanted.is_empty() {
        document.remove("virtual_workspace");
        return Ok(document.to_string());
    }
    let tables = document
        .entry("virtual_workspace")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("virtual_workspace must be [[virtual_workspace]] tables")?;
    tables
        .retain(|table| name_of(table).is_some_and(|name| wanted.iter().any(|vw| vw.name == name)));
    for virtual_workspace in wanted {
        let channels = channel_list(&virtual_workspace.channels);
        let existing = tables
            .iter()
            .position(|table| name_of(table).as_deref() == Some(&virtual_workspace.name));
        match existing.and_then(|index| tables.get_mut(index)) {
            Some(table) => table["channels"] = value(channels),
            None => {
                let mut table = Table::new();
                table["name"] = value(&virtual_workspace.name);
                table["channels"] = value(channels);
                tables.push(table);
            }
        }
    }
    Ok(document.to_string())
}

/// `channels` as a TOML list, one per line when there are several.
fn channel_list(channels: &[ChannelRef]) -> Array {
    let mut array: Array = channels.iter().map(ToString::to_string).collect();
    if channels.len() > 1 {
        for item in array.iter_mut() {
            item.decor_mut().set_prefix("\n    ");
        }
        array.set_trailing_comma(true);
        array.set_trailing("\n");
    }
    // Keep a lone channel on the line: `channels = ["redhat#kuadrant"]`.
    if let Some(Value::String(first)) = array.get_mut(0).filter(|_| channels.len() == 1) {
        first.decor_mut().set_prefix("");
    }
    array
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# My Slack.
workspaces = ["redhat", "cncf"]

# The project I work on.
[[virtual_workspace]]
name = "Kuadrant"
channels = ["redhat#kuadrant", "cncf#kuadrant-dev"]

[[virtual_workspace]]
name = "Old"
channels = ["redhat#old"]
"#;

    fn channel(text: &str) -> ChannelRef {
        ChannelRef::parse(text).unwrap()
    }

    #[test]
    fn it_reads_workspaces_and_virtual_workspaces() {
        let config = Config::parse(SAMPLE).unwrap();
        assert_eq!(config.workspaces, ["redhat", "cncf"]);
        assert_eq!(
            config.virtual_workspaces[0],
            VirtualWorkspace {
                name: "Kuadrant".into(),
                channels: vec![channel("redhat#kuadrant"), channel("cncf#kuadrant-dev")],
            }
        );
        assert_eq!(config.virtual_workspaces.len(), 2);
    }

    #[test]
    fn nothing_is_an_empty_config_and_mistakes_are_errors() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        let error = Config::parse("[[virtual_workspace]]\nname = \"X\"\nchannels = [\"kuadrant\"]")
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("isn't WORKSPACE#CHANNEL"),
            "{error:#}"
        );
        assert!(Config::parse("workspaces = \"redhat\"").is_err());
    }

    #[test]
    fn rewriting_keeps_comments_and_drops_what_went() {
        let mut virtual_workspaces = Config::parse(SAMPLE).unwrap().virtual_workspaces;
        virtual_workspaces.retain(|vw| vw.name != "Old");
        virtual_workspaces[0]
            .channels
            .push(channel("kubernetes#sig-network"));
        virtual_workspaces.push(VirtualWorkspace {
            name: "Gateway API".into(),
            channels: vec![channel("k8s#gw")],
        });

        let text = with_virtual_workspaces(SAMPLE, &virtual_workspaces).unwrap();
        assert!(
            text.starts_with("# My Slack.\nworkspaces = [\"redhat\", \"cncf\"]\n"),
            "{text}"
        );
        assert!(
            text.contains("# The project I work on.\n[[virtual_workspace]]\nname = \"Kuadrant\""),
            "{text}"
        );
        assert!(!text.contains("Old"), "{text}");
        assert!(text.contains("channels = [\"k8s#gw\"]"), "{text}");
        assert_eq!(
            Config::parse(&text).unwrap().virtual_workspaces,
            virtual_workspaces,
            "{text}"
        );
    }

    #[test]
    fn the_last_one_gone_leaves_no_trace() {
        let text = with_virtual_workspaces(SAMPLE, &[]).unwrap();
        assert_eq!(text, "# My Slack.\nworkspaces = [\"redhat\", \"cncf\"]\n");
    }

    #[test]
    fn saving_creates_the_file_and_replaces_it_whole() {
        let directory =
            std::env::temp_dir().join(format!("slackers-config-test-{}", std::process::id()));
        let path = directory.join("nested").join("config.toml");
        let wanted = [VirtualWorkspace {
            name: "K".into(),
            channels: vec![channel("a#b")],
        }];
        save_virtual_workspaces(&path, &wanted).unwrap();
        assert_eq!(Config::load(&path).unwrap().virtual_workspaces, wanted);
        assert!(!path.with_extension("toml.new").exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
