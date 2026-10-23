//! What the app keeps on disk between runs: its settings, the models the user starred,
//! the settings each agent last created a session with, the sessions kept for later with
//! the transcript and last context report of each, and the notes on the Stuff board with
//! the images pasted into them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acp_core::{Harness, Setting};
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::session::{KeptContext, KeptFile, KeptPullRequest, KeptReport, KeptRow};

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
    /// Missing from files written before titles came from the agent.
    #[serde(default)]
    pub(crate) title_source: TitleSource,
    /// Its context window as last counted. Missing until the agent has counted, and
    /// from files written before the info panel.
    #[serde(default)]
    pub(crate) context: Option<KeptContext>,
    /// The pull requests it opened, as they stood when last checked.
    #[serde(default)]
    pub(crate) pull_requests: Vec<KeptPullRequest>,
    /// The files its turns made.
    #[serde(default)]
    pub(crate) files: Vec<KeptFile>,
}

/// Where a session's title came from, which decides whether a title the agent sends
/// replaces it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TitleSource {
    /// The first line of the first prompt, until the agent sends a title of its own.
    Prompt,
    /// The agent's, which the next one it sends replaces.
    Agent,
    /// The user's, as they renamed the session, which the agent never replaces. A
    /// session kept before titles came from the agent counts as this, as nothing tells
    /// whether it was renamed.
    #[default]
    User,
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

/// Holds the transcript of each session kept for later runs, a row a line as JSON, which
/// each turn's end adds to, and beside it the last report of what filled its context
/// window. Each file is named for the session, so a turn writes only its own rows.
const TRANSCRIPTS: &str = "transcripts";

/// The file a session's transcript, or the report beside it, is kept in: named for its
/// agent's `kept_name` and the agent's id for it, which may hold only what a file name can.
fn kept_file(folder: &Path, agent: &str, id: &str, extension: &str) -> PathBuf {
    let id: String = id
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '_',
        })
        .collect();
    folder
        .join(TRANSCRIPTS)
        .join(format!("{agent}-{id}.{extension}"))
}

const TRANSCRIPT: &str = "jsonl";
const REPORT: &str = "context.json";

fn no_config_folder() -> std::io::Error {
    std::io::Error::other("there is no config folder")
}

/// A missing file gives none.
pub(crate) fn load_transcript(agent: &str, id: &str) -> Vec<KeptRow> {
    config_folder()
        .map(|folder| read_transcript(&kept_file(&folder, agent, id, TRANSCRIPT)))
        .unwrap_or_default()
}

/// A line that can't be read, such as one cut short as the app quit, is left out: the
/// agent's replay puts the transcript right once it loads the session.
fn read_transcript(path: &Path) -> Vec<KeptRow> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    bytes
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

fn transcript_lines(rows: &[KeptRow]) -> std::io::Result<Vec<u8>> {
    let mut lines = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut lines, row)?;
        lines.push(b'\n');
    }
    Ok(lines)
}

/// Adds `rows` to the end of the session's transcript, in one write.
pub(crate) fn append_transcript(agent: &str, id: &str, rows: &[KeptRow]) -> std::io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let path = kept_file(
        &config_folder().ok_or_else(no_config_folder)?,
        agent,
        id,
        TRANSCRIPT,
    );
    add_lines(&path, &transcript_lines(rows)?)
}

fn add_lines(path: &Path, lines: &[u8]) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    std::io::Write::write_all(&mut file, lines)
}

/// Keeps `rows` as the session's whole transcript. The new file replaces the old one whole,
/// so a write that fails or is cut short leaves the old one.
pub(crate) fn write_transcript(agent: &str, id: &str, rows: &[KeptRow]) -> std::io::Result<()> {
    let path = kept_file(
        &config_folder().ok_or_else(no_config_folder)?,
        agent,
        id,
        TRANSCRIPT,
    );
    replace_file(&path, &transcript_lines(rows)?)
}

fn replace_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let mut written = path.as_os_str().to_owned();
    written.push(".new");
    std::fs::write(&written, contents)?;
    std::fs::rename(&written, path)
}

/// A file that is missing or can't be read gives none, and the report waits for the agent.
pub(crate) fn load_report(agent: &str, id: &str) -> Option<KeptReport> {
    let path = kept_file(&config_folder()?, agent, id, REPORT);
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub(crate) fn save_report(agent: &str, id: &str, report: &KeptReport) -> std::io::Result<()> {
    let path = kept_file(
        &config_folder().ok_or_else(no_config_folder)?,
        agent,
        id,
        REPORT,
    );
    replace_file(&path, &serde_json::to_vec(report)?)
}

/// Removes what is kept of a deleted session beside the list of sessions.
pub(crate) fn forget_transcript(agent: &str, id: &str) {
    let Some(folder) = config_folder() else {
        return;
    };
    for extension in [TRANSCRIPT, REPORT] {
        match std::fs::remove_file(kept_file(&folder, agent, id, extension)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("couldn't remove a deleted session's kept transcript: {err}")
            }
            _ => {}
        }
    }
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

/// Holds the notes on the Stuff board, newest first, as JSON.
const STUFF: &str = "stuff.json";

/// A note stuck on the Stuff board.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StuffNote {
    /// When it was stuck, in milliseconds since 1970, made unique within the board, so
    /// that a change made by one run of the app finds the same note in another's.
    pub(crate) id: u64,
    pub(crate) text: String,
    /// What was pasted into it besides text, in the order pasted. Written only when there
    /// is something, so a note of text alone reads as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) attachments: Vec<StuffAttachment>,
}

/// An image or a file pasted into a note. A file copied in Finder is kept where it is, by
/// its path; an image pasted as data has been saved in `STUFF_FILES` first.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum StuffAttachment {
    /// Shown as a picture.
    Image { path: PathBuf, mime: String },
    /// Shown by its name and size. A folder has no size.
    File { path: PathBuf, size: Option<u64> },
}

impl StuffAttachment {
    /// A file copied in Finder: a picture if its name says it is a PNG, JPEG, GIF or WebP,
    /// the kinds both gpui and the agents' models take, and any other file by its size.
    pub(crate) fn copied(path: &Path) -> Self {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        let mime = match extension.as_deref() {
            Some("png") => Some("image/png"),
            Some("jpg" | "jpeg") => Some("image/jpeg"),
            Some("gif") => Some("image/gif"),
            Some("webp") => Some("image/webp"),
            _ => None,
        };
        let metadata = std::fs::metadata(path).ok();
        match mime {
            Some(mime) if metadata.as_ref().is_some_and(|metadata| metadata.is_file()) => {
                Self::Image {
                    path: path.to_path_buf(),
                    mime: mime.to_string(),
                }
            }
            _ => Self::File {
                path: path.to_path_buf(),
                size: metadata
                    .filter(|metadata| !metadata.is_dir())
                    .map(|metadata| metadata.len()),
            },
        }
    }

    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::Image { path, .. } | Self::File { path, .. } => path,
        }
    }

    /// The file's own name, or for a pasted image, which has only the name it was saved
    /// under, what it is.
    pub(crate) fn name(&self) -> String {
        if pasted_here(self.path()) {
            return "Pasted image".to_string();
        }
        self.path().file_name().map_or_else(
            || self.path().display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }
}

/// Holds the images pasted into notes as data, each in a file of its own.
const STUFF_FILES: &str = "stuff";

/// Saves an image pasted as data under a name of its own: the time, moved on while a file
/// already has it.
pub(crate) fn save_pasted_image(image: &Image) -> std::io::Result<StuffAttachment> {
    let folder = config_folder()
        .ok_or_else(|| std::io::Error::other("there is no config folder"))?
        .join(STUFF_FILES);
    std::fs::create_dir_all(&folder)?;
    let mut name = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis());
    loop {
        let path = folder.join(format!("{name}.{}", image.format.extension()));
        match std::fs::File::create_new(&path) {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, &image.bytes)?;
                return Ok(StuffAttachment::Image {
                    path,
                    mime: image.format.mime_type().to_string(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => name += 1,
            Err(err) => return Err(err),
        }
    }
}

/// Whether `path` is an image saved by `save_pasted_image`, which the app may remove.
fn pasted_here(path: &Path) -> bool {
    config_folder().is_some_and(|folder| path.starts_with(folder.join(STUFF_FILES)))
}

/// Removes the file of an attachment taken off a note before it was stuck, if it is an
/// image the app saved. A file copied in Finder is the user's, and is left alone.
pub(crate) fn forget_attachment(attachment: &StuffAttachment) {
    let path = attachment.path();
    if pasted_here(path)
        && let Err(err) = std::fs::remove_file(path)
    {
        eprintln!("couldn't remove a pasted image: {err}");
    }
}

/// The file as a whole, so that what is added to it later has a place beside the notes.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct StuffFile {
    notes: Vec<StuffNote>,
}

/// A file that is missing or can't be read gives an empty board. One that can't be read
/// is said, and left alone until the board changes.
pub(crate) fn load_stuff() -> Vec<StuffNote> {
    let Some(folder) = config_folder() else {
        return Vec::new();
    };
    read_stuff(&folder.join(STUFF)).unwrap_or_else(|err| {
        eprintln!("couldn't read the Stuff board: {err}");
        Vec::new()
    })
}

/// An error other than a missing file is passed on, so notes that couldn't be read
/// aren't written over.
fn read_stuff(path: &Path) -> std::io::Result<Vec<StuffNote>> {
    match std::fs::read(path) {
        Ok(json) => Ok(serde_json::from_slice::<StuffFile>(&json)?.notes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Makes `change` to the notes kept in `folder`. Runs of the app changing them at once
/// take turns under a lock, and the new file replaces the old one whole.
pub(crate) fn change_stuff(
    folder: &Path,
    change: impl FnOnce(&mut Vec<StuffNote>),
) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    // Held until the new file is in place.
    let lock = std::fs::File::create(folder.join(format!("{STUFF}.lock")))?;
    lock.lock()?;
    let path = folder.join(STUFF);
    let mut notes = read_stuff(&path)?;
    change(&mut notes);
    let written = folder.join(format!("{STUFF}.new"));
    // Laid out for reading, as notes are the user's own text.
    std::fs::write(&written, serde_json::to_vec_pretty(&StuffFile { notes })?)?;
    std::fs::rename(&written, &path)
}

#[cfg(test)]
mod tests {
    use super::{add_lines, read_transcript, transcript_lines};
    use crate::session::KeptRow;

    #[test]
    fn a_transcript_reads_back_but_for_a_line_cut_short() {
        let folder =
            std::env::temp_dir().join(format!("acp-gui-transcript-{}", std::process::id()));
        let path = folder.join("claude-1.jsonl");
        let _ = std::fs::remove_file(&path);
        let rows = [
            KeptRow::User {
                text: "Fix the build".into(),
                attachments: Vec::new(),
            },
            KeptRow::Agent {
                text: "Done.".into(),
            },
        ];
        add_lines(&path, &transcript_lines(&rows).unwrap()).unwrap();
        // As a write the app quit during would leave it.
        add_lines(&path, br#"{"row":"agent","te"#).unwrap();
        let read = read_transcript(&path);
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(read.len(), 2);
        assert!(matches!(&read[1], KeptRow::Agent { text } if text == "Done."));
    }
}
