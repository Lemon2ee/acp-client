//! The files a session made, as its info panel lists them: PDFs, spreadsheets and CSVs
//! that appeared or changed in its folder while one of its turns ran, and those outside
//! it that a command of the turn named. Neither adapter says what a script writes, so the
//! folder is watched (FSEvents on macOS) from a prompt until its turn ends.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use acp_core::ToolContent;
use gpui_kit::Task;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use serde::{Deserialize, Serialize};

/// What a file must end in to be listed.
const EXTENSIONS: [&str; 5] = ["pdf", "xlsx", "xls", "csv", "tsv"];
/// How long before a turn started a file may have changed and still count, for clocks
/// that round to the second.
const SLACK: Duration = Duration::from_secs(2);

/// A file the session made.
pub(crate) struct MadeFile {
    pub(crate) path: PathBuf,
    /// When it last changed, in seconds since 1970, and how big it was then.
    pub(crate) at: i64,
    pub(crate) size: u64,
    /// It wasn't there when last looked for: moved or deleted since.
    pub(crate) missing: bool,
}

impl MadeFile {
    pub(crate) fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    pub(crate) fn kind(&self) -> FileKind {
        kind(&self.path).unwrap_or(FileKind::Sheet)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileKind {
    /// A spreadsheet or a table: xlsx, xls, csv, tsv.
    Sheet,
    /// A PDF.
    Document,
}

/// A file as kept in the session list between runs.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct KeptFile {
    pub(crate) path: PathBuf,
    pub(crate) at: i64,
    pub(crate) size: u64,
}

/// The files a session made, in the order they first appeared.
#[derive(Default)]
pub(crate) struct SessionFiles {
    pub(crate) list: Vec<MadeFile>,
    /// When the running turn started; none between turns.
    pub(crate) turn_started: Option<SystemTime>,
    /// The paths the running turn's tool calls named, to look at as it ends.
    pub(crate) named: Vec<PathBuf>,
    /// Watches the session's folder from a prompt until a moment after its turn ends.
    pub(crate) watch: Option<Task<()>>,
    /// How many turns have started, so that stopping the watch once one has ended can
    /// tell that another has begun.
    pub(crate) turns: u64,
    /// A turn has just ended: its watch is to stop once its last changes are in.
    pub(crate) ended: bool,
    /// The list changed as a turn ended, and is to be kept.
    pub(crate) found: bool,
}

impl SessionFiles {
    /// Those kept from an earlier run.
    pub(crate) fn restore(kept: &[KeptFile]) -> Self {
        let mut files = Self {
            list: kept
                .iter()
                .map(|kept| MadeFile {
                    path: kept.path.clone(),
                    at: kept.at,
                    size: kept.size,
                    missing: false,
                })
                .collect(),
            ..Self::default()
        };
        files.look_again();
        files
    }

    pub(crate) fn kept(&self) -> Vec<KeptFile> {
        self.list
            .iter()
            .map(|file| KeptFile {
                path: file.path.clone(),
                at: file.at,
                size: file.size,
            })
            .collect()
    }

    /// Notes the files a tool call of the running turn names by their full path.
    pub(crate) fn note_named(&mut self, title: &str, content: &[ToolContent]) {
        if self.turn_started.is_none() {
            return;
        }
        let texts =
            std::iter::once(title).chain(content.iter().filter_map(|content| match content {
                ToolContent::Text(text) => Some(text.as_str()),
                ToolContent::Diff { .. } => None,
            }));
        for path in texts.flat_map(named_paths) {
            if !self.named.contains(&path) {
                self.named.push(path);
            }
        }
    }

    /// Lists the file at `path` if it is one to list and changed during the running turn,
    /// or updates it if it is listed. Whether the list changed.
    pub(crate) fn saw(&mut self, path: &Path) -> bool {
        let Some(started) = self.turn_started else {
            return false;
        };
        if kind(path).is_none() {
            return false;
        }
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        let Ok(modified) = metadata.modified() else {
            return false;
        };
        if !metadata.is_file() || modified + SLACK < started {
            return false;
        }
        let (at, size) = (seconds(modified), metadata.len());
        if let Some(file) = self.list.iter_mut().find(|file| file.path == path) {
            let changed = (file.at, file.size, file.missing) != (at, size, false);
            (file.at, file.size, file.missing) = (at, size, false);
            return changed;
        }
        self.list.push(MadeFile {
            path: path.to_path_buf(),
            at,
            size,
            missing: false,
        });
        true
    }

    /// Looks at the paths the turn named, as it ends.
    pub(crate) fn turn_ended(&mut self) {
        for path in std::mem::take(&mut self.named) {
            self.found |= self.saw(&path);
        }
        self.ended = true;
    }

    /// A turn starts, and the files that change from now on count.
    pub(crate) fn turn_started(&mut self) {
        self.turns += 1;
        self.turn_started = Some(SystemTime::now());
        self.named.clear();
    }

    /// The last turn's changes are all in.
    pub(crate) fn stop_watching(&mut self) {
        self.watch = None;
        self.turn_started = None;
    }

    /// Finds out which files have gone, or come back, since last looked.
    pub(crate) fn look_again(&mut self) {
        for file in &mut self.list {
            match std::fs::metadata(&file.path) {
                Ok(metadata) if metadata.is_file() => {
                    file.missing = false;
                    file.size = metadata.len();
                    if let Ok(modified) = metadata.modified() {
                        file.at = seconds(modified);
                    }
                }
                _ => file.missing = true,
            }
        }
    }
}

/// Whether a file at `path` is one to list, and which kind.
fn kind(path: &Path) -> Option<FileKind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => Some(FileKind::Document),
        _ if EXTENSIONS.contains(&extension.as_str()) => Some(FileKind::Sheet),
        _ => None,
    }
}

/// A change the watch passes on is in a folder people look in, not in what tools keep
/// to themselves: hidden folders such as `.git`, `node_modules`, and cargo's `target*`.
pub(crate) fn watched(path: &Path, folder: &Path) -> bool {
    let Ok(inside) = path.strip_prefix(folder) else {
        return true;
    };
    let mut folders = inside.components().rev().skip(1);
    !folders.any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy();
        name.starts_with('.') || name == "node_modules" || name.starts_with("target")
    })
}

/// Watches `folder` and everything in it, sending each file that is created or changed
/// and is one to list. Watching stops when the watcher is dropped.
pub(crate) fn watch(
    folder: &Path,
    sender: smol::channel::Sender<PathBuf>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if matches!(event.kind, EventKind::Access(_) | EventKind::Remove(_)) {
            return;
        }
        for path in event.paths {
            if kind(&path).is_some() {
                let _ = sender.try_send(path);
            }
        }
    })?;
    watcher.watch(folder, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// The full paths to files to list in a command or its output: those starting at `/` or
/// at `~/`. A path with a space in it isn't found.
fn named_paths(text: &str) -> Vec<PathBuf> {
    let home = dirs::home_dir();
    text.split(|c: char| c.is_whitespace() || "\"'`=<>()[]{},;|".contains(c))
        .filter_map(|word| {
            let word = word.trim_end_matches(['.', ':']);
            kind(Path::new(word))?;
            if word.starts_with('/') {
                Some(PathBuf::from(word))
            } else {
                Some(home.as_ref()?.join(word.strip_prefix("~/")?))
            }
        })
        .collect()
}

fn seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{FileKind, kind, named_paths, watched};
    use std::path::{Path, PathBuf};

    #[test]
    fn only_pdfs_spreadsheets_and_tables_are_listed() {
        assert_eq!(kind(Path::new("out/Report.PDF")), Some(FileKind::Document));
        assert_eq!(kind(Path::new("a.xlsx")), Some(FileKind::Sheet));
        assert_eq!(kind(Path::new("a.tsv")), Some(FileKind::Sheet));
        assert_eq!(kind(Path::new("a.docx")), None);
        assert_eq!(kind(Path::new("csv")), None);
    }

    #[test]
    fn hidden_and_build_folders_are_not_watched() {
        let folder = Path::new("/work");
        assert!(watched(Path::new("/work/out/a.csv"), folder));
        assert!(watched(Path::new("/work/.hidden.csv"), folder));
        assert!(!watched(Path::new("/work/.git/x/a.csv"), folder));
        assert!(!watched(
            Path::new("/work/web/node_modules/p/a.csv"),
            folder
        ));
        assert!(!watched(Path::new("/work/target.nosync/a.pdf"), folder));
        assert!(watched(Path::new("/elsewhere/a.pdf"), folder));
    }

    #[test]
    fn full_paths_are_found_in_commands() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            named_paths("python3 make.py --out=/tmp/x/frames.xlsx && open ~/Desktop/r.pdf."),
            [
                PathBuf::from("/tmp/x/frames.xlsx"),
                home.join("Desktop/r.pdf")
            ]
        );
        assert!(named_paths("cat out/table.csv notes.md").is_empty());
    }
}
