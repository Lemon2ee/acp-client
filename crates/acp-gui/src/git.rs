//! What git says of a session's folder, for the line under the prompt box: the branch,
//! whether the folder is in a linked worktree, how far the branch has gone from its base
//! branch, and the files not committed yet. Read by running `git` in two steps, the
//! branch first as it is quick, then what needs `git diff` and `git status`, which can
//! take seconds in a large repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The repository a folder is in, as `read_head` finds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Repo {
    pub(crate) head: Head,
    /// The main worktree's folder, when the folder is in a linked worktree of it.
    pub(crate) worktree_of: Option<PathBuf>,
    /// What `read_changes` found; None until it has.
    pub(crate) changes: Option<Changes>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Head {
    /// The branch checked out, also in a repository with no commits yet.
    Branch(String),
    /// No branch is checked out: the commit's 7-character hash.
    Detached(String),
}

/// The slower half of what the line shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Changes {
    /// None when there is nothing to show: the branch checked out is the base, or git
    /// couldn't compare them, as in a repository with no commits yet.
    pub(crate) base: Option<Base>,
    /// Files changed and not committed, staged or not.
    pub(crate) uncommitted: usize,
    /// Files git doesn't track and doesn't ignore. A folder of them counts once, as git
    /// lists it so, which spares `git status` walking a large one.
    pub(crate) untracked: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Base {
    /// No origin/HEAD, main or master to compare with.
    Missing,
    /// What the branch's commits changed since it left `name`.
    Diff {
        name: String,
        files: usize,
        added: usize,
        removed: usize,
    },
}

/// Whether the folder is in a repository, and its branch: None outside one, and when git
/// isn't installed. A folder inside a repository counts, as git looks up from it.
pub(crate) fn read_head(folder: &Path) -> Option<Repo> {
    let dirs = git(
        folder,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    )?;
    let mut dirs = dirs.lines().map(PathBuf::from);
    let (git_dir, common_dir) = (dirs.next()?, dirs.next()?);
    // A linked worktree keeps its own git folder under the main one's `.git/worktrees`.
    let worktree_of = (git_dir != common_dir)
        .then(|| common_dir.parent().map(Path::to_path_buf))
        .flatten();
    let head = match git(folder, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Some(branch) => Head::Branch(branch.trim().to_string()),
        None => Head::Detached(
            git(folder, &["rev-parse", "--short=7", "HEAD"])?
                .trim()
                .into(),
        ),
    };
    Some(Repo {
        head,
        worktree_of,
        changes: None,
    })
}

/// The branch's changes against its base and the files not committed. None if git
/// fails, as when the folder has gone.
pub(crate) fn read_changes(folder: &Path, head: &Head) -> Option<Changes> {
    let status = git(folder, &["status", "--porcelain"])?;
    let (uncommitted, untracked) = count_status(&status);
    let base = match base_branch(folder) {
        None => Some(Base::Missing),
        Some(name) if matches!(head, Head::Branch(branch) if is_base(branch, &name)) => None,
        // Three dots: from where the branch left the base, so the base's own later commits
        // don't count. `--numstat` rather than `--shortstat`, whose words git translates.
        Some(name) => git(folder, &["diff", "--numstat", &format!("{name}...HEAD")]).map(|diff| {
            let (files, added, removed) = count_numstat(&diff);
            Base::Diff {
                name,
                files,
                added,
                removed,
            }
        }),
    };
    Some(Changes {
        base,
        uncommitted,
        untracked,
    })
}

/// The branch the folder's work is measured from: the one origin/HEAD points at, such as
/// origin/main, else a main or a master branch.
fn base_branch(folder: &Path) -> Option<String> {
    if let Some(name) = git(folder, &["rev-parse", "--abbrev-ref", "origin/HEAD"]) {
        return Some(name.trim().to_string());
    }
    ["main", "master"].into_iter().find_map(|name| {
        git(
            folder,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ],
        )
        .map(|_| name.to_string())
    })
}

/// The branch checked out is the base, itself or as the remote has it: main for
/// origin/main.
fn is_base(branch: &str, base: &str) -> bool {
    base == branch || base.strip_prefix("origin/") == Some(branch)
}

/// Files, lines added and lines removed in `git diff --numstat` output. A binary file
/// counts as a file with no lines.
fn count_numstat(numstat: &str) -> (usize, usize, usize) {
    numstat.lines().filter(|line| !line.is_empty()).fold(
        (0, 0, 0),
        |(files, added, removed), line| {
            let mut counts = line
                .split('\t')
                .map(|count| count.parse::<usize>().unwrap_or(0));
            let (plus, minus) = (counts.next().unwrap_or(0), counts.next().unwrap_or(0));
            (files + 1, added + plus, removed + minus)
        },
    )
}

/// Files changed and files untracked in `git status --porcelain` output: `??` starts an
/// untracked file's line, and any other line is a change, staged or not.
fn count_status(porcelain: &str) -> (usize, usize) {
    porcelain
        .lines()
        .filter(|line| !line.is_empty())
        .fold((0, 0), |(changed, untracked), line| {
            if line.starts_with("??") {
                (changed, untracked + 1)
            } else {
                (changed + 1, untracked)
            }
        })
}

/// Runs git in the folder and gives what it printed, or None if it failed or isn't
/// installed. `--no-optional-locks` keeps `git status` from taking the index's lock to
/// refresh it, which would make a `git commit` the agent runs meanwhile fail.
fn git(folder: &Path, args: &[&str]) -> Option<String> {
    if !installed() {
        return None;
    }
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(folder)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether git can run without asking anything of the user. Until the Command Line Tools
/// are installed, macOS's `/usr/bin/git` is a stand-in that offers to install them in a
/// dialog, which every read would bring up again; `xcode-select -p` says whether they
/// are, without one. Asked once, as installing them takes a restart of the app to notice.
fn installed() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(|| {
        let on_path = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("git"))
                .find(|git| git.is_file())
        });
        match on_path {
            None => false,
            Some(git) if cfg!(target_os = "macos") && git == Path::new("/usr/bin/git") => {
                Command::new("xcode-select")
                    .arg("-p")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            }
            Some(_) => true,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_counts_files_and_lines() {
        let diff = "12\t3\tsrc/main.rs\n0\t40\tREADME.md\n-\t-\tlogo.png\n";
        assert_eq!(count_numstat(diff), (3, 12, 43));
        assert_eq!(count_numstat(""), (0, 0, 0));
    }

    #[test]
    fn status_tells_untracked_from_changed() {
        let status = " M src/main.rs\nM  Cargo.toml\nR  old.rs -> new.rs\n?? notes/\n?? a.txt\n";
        assert_eq!(count_status(status), (3, 2));
        assert_eq!(count_status(""), (0, 0));
    }

    #[test]
    fn the_base_as_the_remote_has_it_is_the_branch() {
        assert!(is_base("main", "origin/main"));
        assert!(is_base("master", "master"));
        assert!(!is_base("feat/main", "origin/main"));
        assert!(!is_base("main", "upstream/main"));
    }
}
