//! The pull requests a session opened, as its info panel lists them: found in the output
//! of the tool calls that opened them, and how each stands, as the user's own GitHub CLI
//! tells it. The app runs `gh` as the user would in a terminal; gh reads its own login,
//! which the app never sees.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::Instant;

use acp_core::ToolContent;
use gpui_kit::component::diff::DiffFile;
use serde::{Deserialize, Serialize};

/// What `gh pr view` is asked for.
const FIELDS: &str = "number,title,state,isDraft,baseRefName,headRefName,headRefOid,additions,\
                      deletions,files,statusCheckRollup,reviewDecision,latestReviews";
/// gh's exit status when it has no login to use.
const NO_LOGIN: i32 = 4;

/// A pull request the session opened.
pub(crate) struct PullRequest {
    pub(crate) url: String,
    /// The number its address ends in.
    pub(crate) number: u64,
    /// The title the command that opened it gave, shown until gh answers.
    pub(crate) asked_title: Option<String>,
    /// How it stood when gh last answered.
    pub(crate) status: Option<PrStatus>,
    /// Why gh's last answer didn't come.
    pub(crate) problem: Option<GhProblem>,
    /// gh has been asked and hasn't answered.
    pub(crate) checking: bool,
    /// When gh last answered; none until it has in this run, or since a turn ended.
    pub(crate) checked: Option<Instant>,
    /// Each file's diff by its path, as read at the head commit named with them.
    pub(crate) diffs: Option<(String, Rc<HashMap<String, FileDiff>>)>,
    /// `gh pr diff` has been asked and hasn't answered.
    pub(crate) reading_diff: bool,
    /// Why the last diff didn't come, until the user opens a file's diff again.
    pub(crate) diff_problem: Option<GhProblem>,
}

impl PullRequest {
    fn new(url: String, number: u64, asked_title: Option<String>) -> Self {
        Self {
            url,
            number,
            asked_title,
            status: None,
            problem: None,
            checking: false,
            checked: None,
            diffs: None,
            reading_diff: false,
            diff_problem: None,
        }
    }

    /// What is kept of it for later runs.
    pub(crate) fn kept(&self) -> KeptPullRequest {
        KeptPullRequest {
            url: self.url.clone(),
            title: self.asked_title.clone(),
            status: self.status.clone(),
        }
    }

    /// One kept from an earlier run, to be checked again.
    pub(crate) fn restore(kept: &KeptPullRequest) -> Option<Self> {
        let (url, number) = pull_request_at(&kept.url)?;
        let mut pr = Self::new(url, number, kept.title.clone());
        pr.status = kept.status.clone();
        Some(pr)
    }

    /// Its title, as gh last gave it, else as the command did.
    pub(crate) fn title(&self) -> String {
        match (&self.status, &self.asked_title) {
            (Some(status), _) => status.title.clone(),
            (None, Some(title)) => title.clone(),
            (None, None) => format!("Pull request #{}", self.number),
        }
    }

    /// Open or a draft, so its checks and reviews may still change.
    pub(crate) fn is_open(&self) -> bool {
        self.status
            .as_ref()
            .is_none_or(|status| matches!(status.state, PrState::Open | PrState::Draft))
    }
}

/// A pull request as kept in the session list between runs.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct KeptPullRequest {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) status: Option<PrStatus>,
}

/// How a pull request stands, from `gh pr view`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PrStatus {
    pub(crate) title: String,
    pub(crate) state: PrState,
    /// The branch it would merge into, and the one it would merge.
    pub(crate) base: String,
    pub(crate) head: String,
    pub(crate) head_commit: String,
    pub(crate) added: u64,
    pub(crate) removed: u64,
    pub(crate) files: Vec<PrFile>,
    pub(crate) checks: Checks,
    pub(crate) review: Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrState {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Draft => "Draft",
            Self::Merged => "Merged",
            Self::Closed => "Closed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PrFile {
    pub(crate) path: String,
    pub(crate) added: u64,
    pub(crate) removed: u64,
}

/// The pull request's checks, counted: CI runs and commit statuses alike.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Checks {
    pub(crate) total: usize,
    pub(crate) failed: usize,
    pub(crate) running: usize,
}

/// How its checks went, in one word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChecksState {
    Passed,
    Failed,
    Running,
    None,
}

impl Checks {
    pub(crate) fn state(self) -> ChecksState {
        if self.total == 0 {
            ChecksState::None
        } else if self.failed > 0 {
            ChecksState::Failed
        } else if self.running > 0 {
            ChecksState::Running
        } else {
            ChecksState::Passed
        }
    }

    /// "3 checks passed", "1 of 4 failed", "2 of 4 running", "No checks".
    pub(crate) fn text(self) -> String {
        match self.state() {
            ChecksState::None => "No checks".into(),
            ChecksState::Failed => format!("{} of {} failed", self.failed, self.total),
            ChecksState::Running => format!("{} of {} running", self.running, self.total),
            ChecksState::Passed if self.total == 1 => "1 check passed".into(),
            ChecksState::Passed => format!("{} checks passed", self.total),
        }
    }
}

/// Where its reviews stand: GitHub's own decision where the repository requires reviews,
/// else the reviewers' last word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Review {
    Approved,
    ChangesRequested,
    Required,
    /// Reviewers only commented, as bots like Sourcery do.
    Reviewed,
    None,
}

impl Review {
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::Approved => "Approved",
            Self::ChangesRequested => "Changes requested",
            Self::Required => "Review required",
            Self::Reviewed => "Reviewed",
            Self::None => "No reviews",
        }
    }
}

/// Why gh gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GhProblem {
    /// It isn't installed, or isn't logged in: the user can fix that in a terminal.
    NoGh,
    /// Anything else, in gh's words.
    Failed(String),
}

/// The pull requests a session opened, in the order it opened them.
#[derive(Default)]
pub(crate) struct SessionPullRequests {
    pub(crate) list: Vec<PullRequest>,
    /// One has been added since the window last looked, to keep and to check.
    pub(crate) found: bool,
}

impl SessionPullRequests {
    /// Those kept from an earlier run, to be checked again.
    pub(crate) fn restore(kept: &[KeptPullRequest]) -> Self {
        Self {
            list: kept.iter().filter_map(PullRequest::restore).collect(),
            found: false,
        }
    }

    /// Adds the pull request a tool call opened, if it opened one this session doesn't
    /// list yet; `title` is the call's title as it now stands.
    pub(crate) fn note(&mut self, title: &str, content: &[ToolContent]) -> bool {
        let Some((url, number)) = opened(title, content) else {
            return false;
        };
        if self.list.iter().any(|pr| pr.url == url) {
            return false;
        }
        self.list
            .push(PullRequest::new(url, number, asked_title(title)));
        self.found = true;
        true
    }

    /// A turn ended, in which the agent may have pushed to them or merged one, so each
    /// is checked again when next shown.
    pub(crate) fn turn_ended(&mut self) {
        for pr in &mut self.list {
            pr.checked = None;
        }
    }

    /// Those kept for later runs.
    pub(crate) fn kept(&self) -> Vec<KeptPullRequest> {
        self.list.iter().map(PullRequest::kept).collect()
    }

    pub(crate) fn find_mut(&mut self, url: &str) -> Option<&mut PullRequest> {
        self.list.iter_mut().find(|pr| pr.url == url)
    }
}

/// The pull request a tool call opened: a command that runs `gh pr create`, or a GitHub
/// tool that creates one, with the pull request's address in what it printed. Pushing a
/// branch prints `/pull/new/<branch>` for opening one by hand, which isn't one.
fn opened(title: &str, content: &[ToolContent]) -> Option<(String, u64)> {
    let lower = title.to_lowercase();
    if !lower.contains("gh pr create") && !lower.contains("create_pull_request") {
        return None;
    }
    content.iter().find_map(|content| match content {
        ToolContent::Text(text) => text
            .match_indices("https://")
            .find_map(|(at, _)| pull_request_at(&text[at..])),
        ToolContent::Diff { .. } => None,
    })
}

/// The pull request whose address `text` starts with, as that address and its number:
/// `https://<host>/<owner>/<repo>/pull/<number>`, any host, as gh works with GitHub
/// Enterprise too.
fn pull_request_at(text: &str) -> Option<(String, u64)> {
    let end = text
        .find(|c: char| c.is_whitespace() || "\"'`<>()[]{},".contains(c))
        .unwrap_or(text.len());
    let address = &text[..end];
    let mut parts = address.strip_prefix("https://")?.split('/');
    let (host, owner, repo, pull, number) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    let digits = number.len()
        - number
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    if host.is_empty() || owner.is_empty() || repo.is_empty() || pull != "pull" || digits == 0 {
        return None;
    }
    let number = &number[..digits];
    Some((
        format!("https://{host}/{owner}/{repo}/pull/{number}"),
        number.parse().ok()?,
    ))
}

/// The title a `gh pr create` command gives, from `--title` or `-t`.
fn asked_title(command: &str) -> Option<String> {
    let (_, rest) = command
        .split_once("--title")
        .or_else(|| command.split_once(" -t"))?;
    let rest = rest.strip_prefix(['=', ' '])?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
    let title = match quote {
        Some(quote) => rest[1..].split(quote).next()?,
        None => rest.split_whitespace().next()?,
    };
    (!title.is_empty()).then(|| title.to_string())
}

/// How the pull request at `url` stands. Blocks while gh asks GitHub.
pub(crate) fn view(url: &str) -> Result<PrStatus, GhProblem> {
    let output = gh(&["pr", "view", url, "--json", FIELDS])?;
    let pr: GhPullRequest = serde_json::from_slice(&output)
        .map_err(|error| GhProblem::Failed(format!("gh's answer didn't read: {error}")))?;
    Ok(pr.status())
}

/// The pull request's whole diff, as `git diff` writes it. Blocks while gh asks GitHub.
pub(crate) fn read_diff(url: &str) -> Result<String, GhProblem> {
    let output = gh(&["pr", "diff", url, "--color", "never"])?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// Runs the user's `gh`, never waiting on a prompt, and gives what it printed.
fn gh(args: &[&str]) -> Result<Vec<u8>, GhProblem> {
    let output = Command::new("gh")
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| match error.kind() {
            ErrorKind::NotFound => GhProblem::NoGh,
            _ => GhProblem::Failed(error.to_string()),
        })?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let said = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(NO_LOGIN) || said.contains("gh auth login") {
        return Err(GhProblem::NoGh);
    }
    let first = said.lines().find(|line| !line.trim().is_empty());
    Err(GhProblem::Failed(
        first
            .unwrap_or("gh stopped with no message")
            .trim()
            .to_string(),
    ))
}

/// One file's part of a pull request's diff, parsed for gpui-component's `Diff`.
pub(crate) struct FileDiff {
    pub(crate) file: DiffFile,
    /// The rows it shows: one for each line of its hunks, and one more before each hunk
    /// with lines the patch leaves out above it.
    pub(crate) lines: usize,
    pub(crate) gaps: usize,
}

/// Each file's diff in a pull request's whole diff, by the file's path in the head
/// commit, or in the base one for a file it deletes. Each file is parsed on its own, so
/// one the parser refuses leaves the others.
pub(crate) fn file_diffs(patch: &str) -> HashMap<String, FileDiff> {
    let mut diffs = HashMap::new();
    let mut lines = patch.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("diff --git ") {
            continue;
        }
        let mut section = vec![line];
        let (mut rows, mut gaps) = (0, 0);
        // The next line of the old text; none until the first hunk says where it starts.
        let mut next_old: Option<usize> = None;
        while let Some(line) = lines.next_if(|line| !line.starts_with("diff --git ")) {
            section.push(line);
            if let Some(old_start) = hunk_start(line) {
                if old_start > next_old.unwrap_or(1) {
                    gaps += 1;
                }
                next_old = Some(old_start);
                continue;
            }
            let Some(old) = next_old.as_mut() else {
                continue;
            };
            // Some tools trim the space off an empty unchanged line; "\ No newline at
            // end of file" is about the line before.
            match line.as_bytes().first() {
                Some(b'+') => rows += 1,
                Some(b' ' | b'-') | None => {
                    rows += 1;
                    *old += 1;
                }
                _ => {}
            }
        }
        let parsed = DiffFile::parse(&section.join("\n"));
        if let Some(file) = parsed.ok().and_then(|files| files.into_iter().next()) {
            let diff = FileDiff {
                file,
                lines: rows,
                gaps,
            };
            diffs.insert(diff.file.path().to_string(), diff);
        }
    }
    diffs
}

/// Where a hunk starts in the old text, from its `@@ -12,7 +12,9 @@`.
fn hunk_start(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    rest.split_once(" @@")?;
    old.split(',').next()?.parse().ok()
}

/// `gh pr view`'s answer, as far as the panel reads it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPullRequest {
    title: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    files: Vec<GhFile>,
    #[serde(default)]
    status_check_rollup: Vec<GhCheck>,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    latest_reviews: Vec<GhReview>,
}

#[derive(Deserialize)]
struct GhFile {
    path: String,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
}

/// A CI run (`status` and `conclusion`) or a commit status (`state`).
#[derive(Deserialize)]
struct GhCheck {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct GhReview {
    #[serde(default)]
    state: String,
}

impl GhPullRequest {
    fn status(self) -> PrStatus {
        let state = match (self.state.as_str(), self.is_draft) {
            ("MERGED", _) => PrState::Merged,
            ("CLOSED", _) => PrState::Closed,
            (_, true) => PrState::Draft,
            _ => PrState::Open,
        };
        let mut checks = Checks {
            total: self.status_check_rollup.len(),
            ..Checks::default()
        };
        for check in &self.status_check_rollup {
            match check.result() {
                CheckResult::Failed => checks.failed += 1,
                CheckResult::Running => checks.running += 1,
                CheckResult::Passed => {}
            }
        }
        let said = |word: &str| {
            self.latest_reviews
                .iter()
                .any(|review| review.state == word)
        };
        let review = match self.review_decision.as_deref() {
            Some("APPROVED") => Review::Approved,
            Some("CHANGES_REQUESTED") => Review::ChangesRequested,
            Some("REVIEW_REQUIRED") => Review::Required,
            // The repository doesn't require reviews.
            _ if said("CHANGES_REQUESTED") => Review::ChangesRequested,
            _ if said("APPROVED") => Review::Approved,
            _ if said("COMMENTED") => Review::Reviewed,
            _ => Review::None,
        };
        PrStatus {
            title: self.title,
            state,
            base: self.base_ref_name,
            head: self.head_ref_name,
            head_commit: self.head_ref_oid,
            added: self.additions,
            removed: self.deletions,
            files: self
                .files
                .into_iter()
                .map(|file| PrFile {
                    path: file.path,
                    added: file.additions,
                    removed: file.deletions,
                })
                .collect(),
            checks,
            review,
        }
    }
}

enum CheckResult {
    Passed,
    Failed,
    Running,
}

impl GhCheck {
    /// A skipped or neutral run passes, as GitHub's own summary has it; a cancelled one
    /// fails.
    fn result(&self) -> CheckResult {
        if let Some(state) = &self.state {
            return match state.as_str() {
                "SUCCESS" => CheckResult::Passed,
                "PENDING" | "EXPECTED" => CheckResult::Running,
                _ => CheckResult::Failed,
            };
        }
        if self
            .status
            .as_deref()
            .is_some_and(|status| status != "COMPLETED")
        {
            return CheckResult::Running;
        }
        match self.conclusion.as_deref() {
            Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => CheckResult::Passed,
            None | Some("") => CheckResult::Running,
            Some(_) => CheckResult::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{
        Checks, ChecksState, GhPullRequest, PrState, Review, asked_title, file_diffs, opened,
        pull_request_at,
    };
    use acp_core::ToolContent;

    fn text(text: &str) -> Vec<ToolContent> {
        vec![ToolContent::Text(text.into())]
    }

    #[test]
    fn a_created_pull_request_is_found_in_what_its_command_printed() {
        let printed = text(
            "```\nCreating pull request for x into main\n\nhttps://github.com/Lemon2ee/acp-client/pull/65\n```",
        );
        assert_eq!(
            opened("`gh pr create --base main --title \"x\"`", &printed),
            Some(("https://github.com/Lemon2ee/acp-client/pull/65".into(), 65))
        );
        let json = text(
            r#"{"id":1,"url":"https://github.com/o/r/pull/7","html_url":"https://github.com/o/r/pull/7"}"#,
        );
        assert_eq!(
            opened("mcp__github__create_pull_request", &json),
            Some(("https://github.com/o/r/pull/7".into(), 7))
        );
    }

    #[test]
    fn other_commands_and_new_pull_request_links_are_not_pull_requests() {
        let pushed = text(
            "remote: Create a pull request for 'x' on GitHub by visiting:\nremote:   https://github.com/o/r/pull/new/x",
        );
        assert_eq!(opened("git push -u origin x", &pushed), None);
        assert_eq!(opened("gh pr create --fill", &pushed), None);
        let viewed = text("https://github.com/o/r/pull/3");
        assert_eq!(opened("gh pr view 3", &viewed), None);
        assert_eq!(pull_request_at("https://github.com/o/r/pulls/3"), None);
        assert_eq!(
            pull_request_at("https://ghe.example.com/team/app/pull/12/files)."),
            Some(("https://ghe.example.com/team/app/pull/12".into(), 12))
        );
    }

    #[test]
    fn the_command_gives_the_title_until_gh_does() {
        assert_eq!(
            asked_title("gh pr create --base main --title \"acp-gui: fix it\" --body x").as_deref(),
            Some("acp-gui: fix it")
        );
        assert_eq!(
            asked_title("gh pr create -t 'Short one'").as_deref(),
            Some("Short one")
        );
        assert_eq!(
            asked_title("gh pr create --title=word --fill").as_deref(),
            Some("word")
        );
        assert_eq!(asked_title("gh pr create --fill"), None);
    }

    #[test]
    fn checks_and_reviews_read_as_the_design_has_them() {
        let pr: GhPullRequest = serde_json::from_str(
            r#"{"title":"t","state":"OPEN","isDraft":true,"reviewDecision":"",
                "statusCheckRollup":[
                    {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"},
                    {"__typename":"CheckRun","status":"IN_PROGRESS","conclusion":""},
                    {"__typename":"StatusContext","state":"PENDING"},
                    {"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}],
                "latestReviews":[{"state":"COMMENTED"}]}"#,
        )
        .unwrap();
        let status = pr.status();
        assert_eq!(status.state, PrState::Draft);
        assert_eq!(status.checks.state(), ChecksState::Running);
        assert_eq!(status.checks.text(), "2 of 4 running");
        assert_eq!(status.review, Review::Reviewed);
        let failed = Checks {
            total: 4,
            failed: 1,
            running: 1,
        };
        assert_eq!(failed.text(), "1 of 4 failed");
        assert_eq!(Checks::default().text(), "No checks");
        let one = Checks {
            total: 1,
            ..Checks::default()
        };
        assert_eq!(one.text(), "1 check passed");
    }

    #[test]
    fn a_whole_diff_splits_into_its_files() {
        let patch = "diff --git a/src/a.rs b/src/a.rs\n\
                     index 1..2 100644\n\
                     --- a/src/a.rs\n\
                     +++ b/src/a.rs\n\
                     @@ -10,3 +10,4 @@ fn a() {\n \
                     one\n\
                     -two\n\
                     +2\n\
                     +3\n \
                     four\n\
                     @@ -20 +21 @@\n \
                     x\n\
                     \\ No newline at end of file\n\
                     diff --git a/gone.md b/gone.md\n\
                     deleted file mode 100644\n\
                     --- a/gone.md\n\
                     +++ /dev/null\n\
                     @@ -1 +0,0 @@\n\
                     -bye\n";
        let diffs = file_diffs(patch);
        let a = &diffs["src/a.rs"];
        assert_eq!((a.file.additions(), a.file.deletions()), (2, 1));
        // Five lines in the first hunk and one in the second; 9 lines left out before
        // the first and 20 - 13 = 7 between the two.
        assert_eq!((a.lines, a.gaps), (6, 2));
        let gone = &diffs["gone.md"];
        assert_eq!((gone.file.additions(), gone.file.deletions()), (0, 1));
        assert_eq!((gone.lines, gone.gaps), (1, 0));
    }

    #[test]
    fn a_new_file_has_no_gap_above_it() {
        let patch = "diff --git a/new.rs b/new.rs\n\
                     new file mode 100644\n\
                     --- /dev/null\n\
                     +++ b/new.rs\n\
                     @@ -0,0 +1,2 @@\n\
                     +one\n\
                     +two\n";
        let new = &file_diffs(patch)["new.rs"];
        assert_eq!((new.lines, new.gaps), (2, 0));
    }
}
