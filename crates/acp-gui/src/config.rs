//! What the app keeps on disk between runs: its settings, the models the user starred,
//! the settings each agent last created a session with, and the sessions kept for later.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acp_core::{Harness, Setting};
use gpui_kit::*;
use serde::{Deserialize, Serialize};

/// A model the user starred: its agent's `kept_name` and its id. The agent goes by
/// name, not index, so the star holds across runs that start the agents in another
/// order, or start one of them by the `claude` or `codex` argument.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Favorite {
    pub(crate) agent: String,
    pub(crate) model: String,
}

/// What the settings page sets, kept on disk as `SETTINGS`. A global, so the window hears
/// of each change wherever it is made.
#[derive(Clone, Serialize, Deserialize)]
// A setting the file doesn't name, such as one added since it was written, takes its
// default.
#[serde(default)]
pub(crate) struct AppSettings {
    /// Light or dark, or as the system is set.
    pub(crate) appearance: Appearance,
    /// Whether the window blurs what is behind it, where `WINDOW_BLUR` says it can.
    pub(crate) window_blur: bool,
    /// Whether the system-wide shortcut brings the window to the front, where
    /// `GLOBAL_SHORTCUT` says there can be one.
    pub(crate) bring_to_front: bool,
    /// That shortcut, as gpui writes a keystroke (`ctrl-alt-a`). None until one is
    /// recorded, and kept while the shortcut is off.
    pub(crate) bring_to_front_shortcut: Option<String>,
    /// Whether pressing it with the window in front already hides the app.
    pub(crate) hide_when_in_front: bool,
}

impl Global for AppSettings {}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            // The translucent window is the app's own look; the switch turns it off.
            window_blur: true,
            // Nothing is taken from other apps until the user asks.
            bring_to_front: false,
            bring_to_front_shortcut: None,
            hide_when_in_front: false,
        }
    }
}

impl AppSettings {
    /// Makes `change` to the settings and saves them. Another run of the app may have
    /// changed the file since this one read it, so the change is made to the settings
    /// on disk, and this run takes those up.
    pub(crate) fn change(cx: &mut App, change: impl Fn(&mut AppSettings)) {
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| change_settings(&folder, &change));
        let settings = changed.unwrap_or_else(|err| {
            eprintln!("couldn't save the settings: {err}");
            // The change still holds for this run.
            let mut settings = cx.global::<AppSettings>().clone();
            change(&mut settings);
            settings
        });
        cx.set_global(settings);
    }
}

/// Whether the app is light or dark.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Appearance {
    /// As the system is set, and again whenever that setting changes.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub(crate) const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub(crate) fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::System => IconName::Monitor,
            Self::Light => IconName::Sun,
            Self::Dark => IconName::Moon,
        }
    }
}

/// The folder for the app's own files.
pub(crate) fn config_folder() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("acp-client"))
}

/// Holds the models the user starred, one a line: the agent's `kept_name`, a tab, the
/// model's id.
const FAVORITES: &str = "favorite-models";

pub(crate) fn load_favorites() -> Vec<Favorite> {
    config_folder()
        .and_then(|folder| read_favorites(&folder.join(FAVORITES)).ok())
        .unwrap_or_default()
}

/// An error other than a missing file is passed on, so a list that couldn't be read
/// isn't written over.
fn read_favorites(path: &Path) -> std::io::Result<Vec<Favorite>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(agent, model)| Favorite {
                agent: agent.into(),
                model: model.into(),
            })
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Makes `change` to the favorites kept in `folder`, and returns the list as it then
/// stands. Windows changing it at once take turns under a lock, and the new list
/// replaces the old one whole, so a write that fails or is cut short leaves the old list.
pub(crate) fn change_favorites(
    folder: &Path,
    change: impl Fn(&mut Vec<Favorite>),
) -> std::io::Result<Vec<Favorite>> {
    std::fs::create_dir_all(folder)?;
    // Held until the new list is in place.
    let lock = std::fs::File::create(folder.join(format!("{FAVORITES}.lock")))?;
    lock.lock()?;
    let path = folder.join(FAVORITES);
    let mut favorites = read_favorites(&path)?;
    change(&mut favorites);
    let written = folder.join(format!("{FAVORITES}.new"));
    std::fs::write(
        &written,
        favorites
            .iter()
            .map(|favorite| format!("{}\t{}\n", favorite.agent, favorite.model))
            .collect::<String>(),
    )?;
    std::fs::rename(&written, &path)?;
    Ok(favorites)
}

/// Holds the settings each agent last created a session with, by `kept_name`, as JSON.
const LAST_SETTINGS: &str = "last-settings";

/// The harness an agent is, whether the window started it as that or by the `claude`
/// or `codex` argument. None for any other command.
pub(crate) fn harness_of(name: &str) -> Option<Harness> {
    Harness::ALL
        .into_iter()
        .find(|harness| harness.name().eq_ignore_ascii_case(name))
}

/// The name an agent's settings and favorites are kept under on disk: its harness's.
/// None for any other command, which is all that names such an agent and may carry a
/// secret, such as a key, so nothing of it is written.
pub(crate) fn kept_name(name: &str) -> Option<&'static str> {
    harness_of(name).map(Harness::name)
}

/// A file that is missing or can't be read gives none, and the buttons wait for the
/// agent as they did before there was one.
pub(crate) fn load_last_settings() -> HashMap<String, Vec<Setting>> {
    config_folder()
        .map(|folder| read_last_settings(&folder.join(LAST_SETTINGS)))
        .unwrap_or_default()
}

fn read_last_settings(path: &Path) -> HashMap<String, Vec<Setting>> {
    std::fs::read(path)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
        .unwrap_or_default()
}

/// Keeps `settings` as the agent's last in `folder`, beside the other agents'. Windows
/// saving at once take turns under a lock, and the new file replaces the old one whole.
pub(crate) fn save_last_settings(
    folder: &Path,
    agent: &str,
    settings: &[Setting],
) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    // Held until the new file is in place.
    let lock = std::fs::File::create(folder.join(format!("{LAST_SETTINGS}.lock")))?;
    lock.lock()?;
    let path = folder.join(LAST_SETTINGS);
    // Unlike the added models, nothing is lost by starting a file that can't be read
    // over: the agents give their settings again on the next run.
    let mut all = read_last_settings(&path);
    all.insert(agent.to_string(), settings.to_vec());
    let written = folder.join(format!("{LAST_SETTINGS}.new"));
    std::fs::write(&written, serde_json::to_vec(&all)?)?;
    std::fs::rename(&written, &path)
}

/// Holds the sessions kept for later runs, oldest first, as JSON.
const SESSIONS: &str = "sessions";

/// A session kept for later runs, which the agent loads by its id.
#[derive(Serialize, Deserialize)]
pub(crate) struct SavedSession {
    /// Its agent's `kept_name`.
    pub(crate) agent: String,
    /// The agent's id for it.
    pub(crate) id: String,
    pub(crate) title: String,
    /// The folder the agent created it in, which loading it needs.
    pub(crate) cwd: PathBuf,
    /// In the sidebar's Archived list. Missing from files written before there was one.
    #[serde(default)]
    pub(crate) archived: bool,
}

/// A file that is missing or can't be read gives none.
pub(crate) fn load_saved_sessions() -> Vec<SavedSession> {
    config_folder()
        .and_then(|folder| read_saved_sessions(&folder.join(SESSIONS)).ok())
        .unwrap_or_default()
}

/// An error other than a missing file is passed on, so sessions that couldn't be read
/// aren't written over.
fn read_saved_sessions(path: &Path) -> std::io::Result<Vec<SavedSession>> {
    match std::fs::read(path) {
        Ok(json) => Ok(serde_json::from_slice(&json)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Makes `change` to the sessions kept in `folder`. Windows changing them at once take
/// turns under a lock, and the new file replaces the old one whole.
pub(crate) fn change_saved_sessions(
    folder: &Path,
    change: impl FnOnce(&mut Vec<SavedSession>),
) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    // Held until the new file is in place.
    let lock = std::fs::File::create(folder.join(format!("{SESSIONS}.lock")))?;
    lock.lock()?;
    let path = folder.join(SESSIONS);
    let mut sessions = read_saved_sessions(&path)?;
    change(&mut sessions);
    let written = folder.join(format!("{SESSIONS}.new"));
    std::fs::write(&written, serde_json::to_vec(&sessions)?)?;
    std::fs::rename(&written, &path)
}

/// Holds what the settings page sets, as JSON.
pub(crate) const SETTINGS: &str = "settings.json";

/// A missing file gives the defaults. So does one that can't be read, which is said.
pub(crate) fn load_settings() -> AppSettings {
    let Some(folder) = config_folder() else {
        return AppSettings::default();
    };
    read_settings(&folder.join(SETTINGS)).unwrap_or_else(|err| {
        eprintln!("couldn't read the settings: {err}");
        AppSettings::default()
    })
}

/// An error other than a missing file is passed on, so a file that couldn't be read,
/// such as one edited by hand into bad JSON, isn't written over.
fn read_settings(path: &Path) -> std::io::Result<AppSettings> {
    match std::fs::read(path) {
        Ok(json) => Ok(serde_json::from_slice(&json)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AppSettings::default()),
        Err(err) => Err(err),
    }
}

/// Makes `change` to the settings kept in `folder`, and returns them as they then stand.
/// Runs of the app changing them at once take turns under a lock, and the new file
/// replaces the old one whole.
fn change_settings(
    folder: &Path,
    change: impl Fn(&mut AppSettings),
) -> std::io::Result<AppSettings> {
    std::fs::create_dir_all(folder)?;
    // Held until the new file is in place.
    let lock = std::fs::File::create(folder.join(format!("{SETTINGS}.lock")))?;
    lock.lock()?;
    let path = folder.join(SETTINGS);
    let mut settings = read_settings(&path)?;
    change(&mut settings);
    let written = folder.join(format!("{SETTINGS}.new"));
    // Laid out for reading, as it is a file a person may edit.
    std::fs::write(&written, serde_json::to_vec_pretty(&settings)?)?;
    std::fs::rename(&written, &path)?;
    Ok(settings)
}
