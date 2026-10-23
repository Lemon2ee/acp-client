//! A tool call as its row keeps it: what the agent sent, made ready to draw once rather
//! than on every frame, and how long the call ran.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use acp_core::{PermissionRequest, ToolContent, ToolKind, ToolLocation, ToolStatus};
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use similar::{ChangeTag, TextDiff};

/// How many unchanged lines a diff keeps around each change; longer runs between two
/// changes fold into one line.
const DIFF_CONTEXT: usize = 3;
/// How long working out a diff may take before it settles for a coarser one, so an edit
/// to a long file can't stall the window.
const DIFF_TIMEOUT: Duration = Duration::from_millis(50);
/// What a tab in a command's output or a file stands for; the line is drawn as text.
const TAB: &str = "    ";
/// How many characters of a line the number column and a diff's sign column take, for
/// finding the line that draws widest: 36px and 12px beside 12px monospace text.
const NUMBER_CHARS: usize = 5;
const SIGN_CHARS: usize = 2;

pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) kind: ToolKind,
    pub(crate) status: ToolStatus,
    /// The files it works on, as the agent names them.
    pub(crate) locations: Vec<ToolLocation>,
    pub(crate) content: Vec<Shown>,
    /// A hash of the content as the agent last sent it, so an update repeating it, as a
    /// permission request and the call's end both do, isn't worked out again.
    sent: Option<u64>,
    /// Lines its edits add and remove, over all its diffs.
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// When it came in, in a live turn, or when the user allowed it to run. ACP sends no
    /// times, so a call a loaded session replays has none and shows no duration.
    pub(crate) started: Option<Instant>,
    /// How long it ran, from starting to first finishing.
    pub(crate) took: Option<Duration>,
    /// The user has opened it to show its content.
    pub(crate) open: bool,
    /// The agent's request for permission to run it, which the call shows as a card in
    /// its row's place until answered. Boxed, as few calls have one.
    pub(crate) ask: Option<Box<Ask>>,
}

/// A permission request on a tool call, and how it went.
pub(crate) struct Ask {
    /// Kept once answered, for the card to fold away with its options.
    pub(crate) request: PermissionRequest,
    /// One for each option, so the keys can move between them.
    pub(crate) focus: Vec<FocusHandle>,
    /// The option Enter answers with from the prompt, which Up and Down move.
    pub(crate) lit: usize,
    pub(crate) answer: Answer,
    /// The option picked holds for later calls like this one too.
    pub(crate) always: bool,
    /// What the card shows of the call when its content doesn't: the command a shell
    /// call would run, which is its title, or the title of a call with nothing else.
    pub(crate) preview: Option<Code>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    Open,
    Allowed,
    /// ACP reports a rejected call as failed; this keeps it from showing as one.
    Rejected,
    /// The turn was stopped or ended before an answer.
    Cancelled,
}

/// One piece of a tool call's content, ready to draw.
pub(crate) enum Shown {
    /// A fenced code block, as Claude's adapter sends a command's output or a file it read.
    Code(Code),
    /// Any other text, drawn as markdown, with the text itself for copying.
    Text(Entity<TextViewState>, SharedString),
    /// A change to one file.
    Diff(Diff),
}

pub(crate) struct Code {
    /// Shared with the list that draws only the lines in view.
    pub(crate) lines: Rc<[CodeLine]>,
    /// The line that draws widest, which sets how far the box scrolls sideways.
    pub(crate) widest: usize,
    pub(crate) copy: SharedString,
}

pub(crate) struct CodeLine {
    /// The number the agent gave the line, as Claude's Read does a file's lines.
    pub(crate) number: Option<SharedString>,
    pub(crate) text: SharedString,
}

pub(crate) struct Diff {
    pub(crate) path: PathBuf,
    pub(crate) lines: Rc<[DiffLine]>,
    pub(crate) widest: usize,
    /// Its lines carry their numbers in the file, which only a hunk with its start or a
    /// new file gives.
    pub(crate) numbered: bool,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// The lines as they show, marked with `+` and `-`.
    pub(crate) copy: SharedString,
}

pub(crate) enum DiffLine {
    /// A line of the file with its number: in the new text, or in the old one for a line
    /// the change removes.
    Line {
        change: ChangeTag,
        number: SharedString,
        text: SharedString,
    },
    /// Unchanged lines between two changes, folded away.
    Folded(usize),
}

impl ToolCall {
    /// A call that just came in, with no content yet. `live` is whether it came in a live
    /// turn, which times it.
    pub(crate) fn new(
        id: String,
        title: String,
        kind: ToolKind,
        status: ToolStatus,
        live: bool,
    ) -> Self {
        Self {
            id,
            title,
            kind,
            status,
            locations: Vec::new(),
            content: Vec::new(),
            sent: None,
            added: 0,
            removed: 0,
            // One that arrives finished ran before it was shown, for no time we know.
            started: (live && !status.finished()).then(Instant::now),
            took: None,
            open: false,
            ask: None,
        }
    }

    /// The card asking for permission to run it is up.
    pub(crate) fn asking(&self) -> bool {
        self.ask
            .as_ref()
            .is_some_and(|ask| ask.answer == Answer::Open)
    }

    /// No permission request stands in its way: none came, or the user allowed it.
    pub(crate) fn may_run(&self) -> bool {
        self.ask
            .as_ref()
            .is_none_or(|ask| ask.answer == Answer::Allowed)
    }

    /// The user turned its permission request down.
    pub(crate) fn rejected(&self) -> bool {
        self.ask
            .as_ref()
            .is_some_and(|ask| ask.answer == Answer::Rejected)
    }

    /// It failed on its own, rather than by being turned down.
    pub(crate) fn failed(&self) -> bool {
        self.status == ToolStatus::Failed && !self.rejected()
    }

    /// Takes a permission request for it, which asks until answered.
    pub(crate) fn ask(&mut self, request: PermissionRequest, cx: &mut App) {
        let focus = request.options.iter().map(|_| cx.focus_handle()).collect();
        let bare = self.content.is_empty() && self.locations.is_empty();
        let preview = (self.kind == ToolKind::Execute || bare).then(|| plain_code(&self.title));
        self.ask = Some(Box::new(Ask {
            request,
            focus,
            lit: 0,
            answer: Answer::Open,
            always: false,
            preview,
        }));
    }

    /// Takes the fields an update names; the content and the locations replace the ones
    /// before as a whole.
    pub(crate) fn update(
        &mut self,
        title: Option<String>,
        kind: Option<ToolKind>,
        status: Option<ToolStatus>,
        content: Option<Vec<ToolContent>>,
        locations: Option<Vec<ToolLocation>>,
        cx: &mut App,
    ) {
        if let Some(title) = title {
            self.title = title;
        }
        if let Some(kind) = kind {
            self.kind = kind;
        }
        if let Some(status) = status {
            self.status = status;
            if status.finished() && self.took.is_none() {
                self.took = self.started.map(|started| started.elapsed());
            }
        }
        // First, as a diff's line numbers come from them.
        let moved = locations
            .as_ref()
            .is_some_and(|locations| *locations != self.locations);
        if let Some(locations) = locations {
            self.locations = locations;
        }
        if let Some(content) = content {
            let hash = {
                let mut hasher = DefaultHasher::new();
                content.hash(&mut hasher);
                hasher.finish()
            };
            if self.sent != Some(hash) || moved {
                self.sent = Some(hash);
                self.set_content(content, cx);
            }
        }
    }

    /// How long it has been running, since it came in or was allowed, while it hasn't
    /// finished. Whether it runs at all, the caller knows from the turn and the call's
    /// permission request.
    pub(crate) fn running_for(&self) -> Option<Duration> {
        let started = self.started.filter(|_| self.took.is_none())?;
        (!self.status.finished()).then(|| started.elapsed())
    }

    fn set_content(&mut self, content: Vec<ToolContent>, cx: &mut App) {
        let mut hunks = Hunks::new(&self.locations);
        self.content = content
            .into_iter()
            .filter_map(|content| match content {
                ToolContent::Text(text) => shown_text(text, cx),
                ToolContent::Diff { path, old, new } => {
                    Some(Shown::Diff(hunks.diff(path, old.as_deref(), &new)))
                }
            })
            .collect();
        (self.added, self.removed) =
            self.content
                .iter()
                .fold((0, 0), |(a, r), shown| match shown {
                    Shown::Diff(diff) => (a + diff.added, r + diff.removed),
                    _ => (a, r),
                });
    }
}

/// A call's diffs, worked out in the order they come. Claude's adapter sends an edit as
/// the snippet it changes, with no line, and once done as one diff for each hunk, with the
/// hunk's first line in the new file as the location at the same place. The lines earlier
/// hunks added or removed put the old file's numbers that far off the new one's.
struct Hunks<'a> {
    locations: &'a [ToolLocation],
    seen: usize,
    /// The file the last diff was to, and how many lines its diffs so far added, less the
    /// ones they removed.
    shift: Option<(PathBuf, isize)>,
}

impl<'a> Hunks<'a> {
    fn new(locations: &'a [ToolLocation]) -> Self {
        Self {
            locations,
            seen: 0,
            shift: None,
        }
    }

    fn diff(&mut self, path: PathBuf, old: Option<&str>, new: &str) -> Diff {
        let line = self
            .locations
            .get(self.seen)
            .filter(|location| location.path == path)
            .and_then(|location| location.line);
        self.seen += 1;
        let earlier = match &self.shift {
            Some((file, lines)) if *file == path => *lines,
            _ => 0,
        };
        let start = match (old, line) {
            // A new file is all there is of it.
            (None, _) => Some((1, 1)),
            (Some(_), Some(line)) => {
                let line = line as usize;
                Some((line.saturating_add_signed(-earlier).max(1), line))
            }
            (Some(_), None) => None,
        };
        let diff = diff(path.clone(), old.unwrap_or(""), new, start);
        let lines = earlier + diff.added as isize - diff.removed as isize;
        self.shift = Some((path, lines));
        diff
    }
}

/// Text as it shows: one fenced code block as its lines, anything else as markdown.
/// Blank text shows nothing.
fn shown_text(text: String, cx: &mut App) -> Option<Shown> {
    if let Some(body) = fenced(&text) {
        let lines = code_lines(body);
        if lines.iter().all(|line| line.text.trim().is_empty()) {
            return None;
        }
        return Some(Shown::Code(code(lines)));
    }
    if text.trim().is_empty() {
        return None;
    }
    let view = cx.new(|cx| TextViewState::markdown(&text, cx));
    Some(Shown::Text(view, text.into()))
}

/// Text as lines of code, with no numbers.
fn plain_code(text: &str) -> Code {
    code(text.lines().map(plain_line).collect())
}

fn code(lines: Vec<CodeLine>) -> Code {
    let copy = lines
        .iter()
        .map(|line| line.text.as_ref())
        .collect::<Vec<_>>()
        .join("\n");
    let weights = lines
        .iter()
        .map(|line| line.text.chars().count() + line.number.as_ref().map_or(0, |_| NUMBER_CHARS));
    Code {
        widest: widest(weights),
        lines: lines.into(),
        copy: copy.into(),
    }
}

fn plain_line(line: &str) -> CodeLine {
    CodeLine {
        number: None,
        text: line.replace('\t', TAB).into(),
    }
}

/// The body of `text` if it is one fenced code block and nothing else.
fn fenced(text: &str) -> Option<&str> {
    let text = text.trim();
    let marker = text.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let fence_len = text.chars().take_while(|c| *c == marker).count();
    if fence_len < 3 {
        return None;
    }
    let fence = &text[..fence_len];
    // The first line is the fence and its info string, such as `console`.
    let (_, rest) = text.split_once('\n')?;
    let body = rest.strip_suffix(fence)?;
    // A fence line inside it would make it more than one block. The adapters lengthen
    // the fence past any run of backticks that starts a line of the text.
    if body.lines().any(|line| line.starts_with(fence)) {
        return None;
    }
    Some(body.strip_suffix('\n').unwrap_or(body))
}

/// A code block's lines, with their numbers split off when every line has one, as
/// Claude's Read gives them: the number, maybe padded, then a tab or an arrow. The last
/// line may go without, as Read's note that it cut the file short does.
fn code_lines(body: &str) -> Vec<CodeLine> {
    let lines: Vec<&str> = body.lines().collect();
    let Some((last, rest)) = lines.split_last() else {
        return Vec::new();
    };
    let numbered: Option<Vec<CodeLine>> = rest.iter().map(|line| numbered_line(line)).collect();
    match numbered {
        Some(mut numbered) if !rest.is_empty() || numbered_line(last).is_some() => {
            numbered.push(numbered_line(last).unwrap_or_else(|| plain_line(last)));
            numbered
        }
        _ => lines.into_iter().map(plain_line).collect(),
    }
}

fn numbered_line(line: &str) -> Option<CodeLine> {
    let trimmed = line.trim_start();
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    let rest = &trimmed[digits..];
    let text = rest.strip_prefix('\t').or_else(|| rest.strip_prefix('→'))?;
    (digits > 0).then(|| CodeLine {
        number: Some(trimmed[..digits].to_string().into()),
        text: text.replace('\t', TAB).into(),
    })
}

/// The change from `old` to `new` as unified diff lines: each change with a few unchanged
/// lines around it, the longer unchanged runs between changes folded into one line.
/// `start` is where the two texts begin in the file, as line numbers of the old and the
/// new one; without it the lines go unnumbered, as a snippet's numbers would be wrong.
fn diff(path: PathBuf, old: &str, new: &str, start: Option<(usize, usize)>) -> Diff {
    // A last line with no line break would differ from the same line with one.
    let ended = |text: &str| {
        if text.is_empty() || text.ends_with('\n') {
            text.to_string()
        } else {
            format!("{text}\n")
        }
    };
    let (old, new) = (ended(old), ended(new));
    let text_diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(&old, &new);
    let mut lines = Vec::new();
    let (mut added, mut removed) = (0, 0);
    let mut last_old_end = None;
    for group in text_diff.grouped_ops(DIFF_CONTEXT) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        if let Some(end) = last_old_end {
            let skipped = first.old_range().start - end;
            if skipped > 0 {
                lines.push(DiffLine::Folded(skipped));
            }
        }
        last_old_end = Some(last.old_range().end);
        for op in &group {
            for change in text_diff.iter_changes(op) {
                let number = start.and_then(|(old_start, new_start)| match change.tag() {
                    ChangeTag::Delete => change.old_index().map(|n| n + old_start),
                    _ => change.new_index().map(|n| n + new_start),
                });
                match change.tag() {
                    ChangeTag::Insert => added += 1,
                    ChangeTag::Delete => removed += 1,
                    ChangeTag::Equal => {}
                }
                let text = change.value().trim_end_matches(['\n', '\r']);
                lines.push(DiffLine::Line {
                    change: change.tag(),
                    number: number.map_or_else(String::new, |n| n.to_string()).into(),
                    text: text.replace('\t', TAB).into(),
                });
            }
        }
    }
    finish(path, lines, start.is_some(), added, removed)
}

/// A file's part of a unified diff as `git diff` writes it, from its first `@@` line on:
/// numbered from where each hunk starts, with the unchanged lines before and between
/// hunks folded. The patch doesn't say how many lines follow its last hunk, so none
/// fold there.
pub(crate) fn patch_diff(path: PathBuf, patch: &[&str]) -> Diff {
    let mut lines = Vec::new();
    let (mut added, mut removed) = (0, 0);
    // The next line of each text; none until the first hunk says where it starts.
    let mut next: Option<(usize, usize)> = None;
    for line in patch {
        if let Some((old_start, new_start)) = hunk_start(line) {
            let shown = next.map_or(1, |(old, _)| old);
            let skipped = old_start.saturating_sub(shown);
            if skipped > 0 {
                lines.push(DiffLine::Folded(skipped));
            }
            next = Some((old_start, new_start));
            continue;
        }
        let Some((old, new)) = next.as_mut() else {
            continue;
        };
        // Some tools trim the space off an empty unchanged line.
        let (change, text) = match line.split_at_checked(1) {
            Some(("+", text)) => (ChangeTag::Insert, text),
            Some(("-", text)) => (ChangeTag::Delete, text),
            Some((" ", text)) => (ChangeTag::Equal, text),
            // "\ No newline at end of file", about the line before.
            Some(("\\", _)) => continue,
            _ if line.is_empty() => (ChangeTag::Equal, ""),
            _ => continue,
        };
        let number = match change {
            ChangeTag::Insert => {
                added += 1;
                *new += 1;
                *new - 1
            }
            ChangeTag::Delete => {
                removed += 1;
                *old += 1;
                *old - 1
            }
            ChangeTag::Equal => {
                *old += 1;
                *new += 1;
                *new - 1
            }
        };
        lines.push(DiffLine::Line {
            change,
            number: number.to_string().into(),
            text: text.trim_end_matches('\r').replace('\t', TAB).into(),
        });
    }
    finish(path, lines, true, added, removed)
}

/// Where a hunk starts in the old text and the new one, from its `@@ -12,7 +12,9 @@`.
fn hunk_start(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let start = |range: &str| range.split(',').next()?.parse().ok();
    Some((start(old)?, start(new)?))
}

/// The diff of `lines`, with what copying it gives and the line that draws widest.
fn finish(
    path: PathBuf,
    lines: Vec<DiffLine>,
    numbered: bool,
    added: usize,
    removed: usize,
) -> Diff {
    let copy = lines
        .iter()
        .map(|line| match line {
            DiffLine::Line { change, text, .. } => format!("{}{text}", change),
            DiffLine::Folded(count) => folded_text(*count),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let columns = SIGN_CHARS + if numbered { NUMBER_CHARS } else { 0 };
    let widest = widest(lines.iter().map(|line| match line {
        DiffLine::Line { text, .. } => text.chars().count() + columns,
        DiffLine::Folded(count) => folded_text(*count).chars().count(),
    }));
    Diff {
        path,
        lines: lines.into(),
        widest,
        numbered,
        added,
        removed,
        copy: copy.into(),
    }
}

/// How a diff shows `count` unchanged lines folded away.
pub(crate) fn folded_text(count: usize) -> String {
    format!("⋯ {count} unchanged lines")
}

/// The index of the line with the most characters, counting the columns drawn beside
/// its text. The text is drawn in a monospace font, so that line draws widest, give or
/// take wide characters.
fn widest(weights: impl Iterator<Item = usize>) -> usize {
    weights
        .enumerate()
        .max_by_key(|(_, weight)| *weight)
        .map_or(0, |(ix, _)| ix)
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{Diff, DiffLine, Hunks, ToolLocation, code_lines, diff, fenced};
    use similar::ChangeTag;

    #[test]
    fn a_fenced_block_shows_as_its_lines() {
        assert_eq!(fenced("```console\nok\n```"), Some("ok"));
        assert_eq!(fenced("\n````\na\n```\nb\n````\n"), Some("a\n```\nb"));
        assert_eq!(fenced("```\n```"), Some(""));
        // Prose, or a block with text after it, is markdown.
        assert_eq!(fenced("Fetched the page"), None);
        assert_eq!(fenced("```\na\n```\nmore"), None);
        assert_eq!(fenced("```\na\n```\n\n```\nb\n```"), None);
    }

    #[test]
    fn numbered_lines_split_off_their_numbers() {
        let lines = code_lines("   140\tpub enum ToolStatus {\n   141\t\tPending,");
        assert_eq!(lines[0].number.as_deref(), Some("140"));
        assert_eq!(lines[0].text.as_ref(), "pub enum ToolStatus {");
        assert_eq!(lines[1].text.as_ref(), "    Pending,");
        let arrows = code_lines("     1→fn main() {}");
        assert_eq!(arrows[0].number.as_deref(), Some("1"));
        // Output with a number on only some lines keeps them in the text.
        let output = code_lines("12 files\n3\tchanged");
        assert!(output.iter().all(|line| line.number.is_none()));
        assert_eq!(output[1].text.as_ref(), "3    changed");
        // Read's note that it cut the file short has no number.
        let cut = code_lines("1\ta\n2\tb\n[File truncated: showing 2 of 9 lines]");
        assert_eq!(cut[1].number.as_deref(), Some("2"));
        assert!(cut[2].number.is_none());
        // One line of output without a number is just output.
        assert!(code_lines("done").iter().all(|line| line.number.is_none()));
    }

    #[test]
    fn a_diff_keeps_context_and_folds_the_rest() {
        let old: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let new = old
            .replace("line 2\n", "line two\n")
            .replace("line 18\n", "line 18\nline 18b\n");
        let diff = diff("a.rs".into(), &old, &new, Some((1, 1)));
        assert_eq!((diff.added, diff.removed), (2, 1));
        let folded: Vec<_> = diff
            .lines
            .iter()
            .filter_map(|line| match line {
                DiffLine::Folded(count) => Some(*count),
                DiffLine::Line { .. } => None,
            })
            .collect();
        // Lines 1 to 5 around the first change, 16 to 20 around the second.
        assert_eq!(folded, [10]);
        let DiffLine::Line { change, number, .. } = &diff.lines[1] else {
            panic!("the second line is a change");
        };
        assert_eq!((*change, number.as_ref()), (ChangeTag::Delete, "2"));
    }

    #[test]
    fn a_new_file_is_all_added() {
        let diff = diff("new.rs".into(), "", "a\nb\n", Some((1, 1)));
        assert_eq!((diff.added, diff.removed), (2, 0));
        assert_eq!(diff.copy.as_ref(), "+a\n+b");
    }

    #[test]
    fn a_last_line_without_a_break_is_no_change() {
        let diff = diff("a.rs".into(), "a\nb", "a\nb\nc", None);
        assert_eq!((diff.added, diff.removed), (1, 0));
        assert_eq!(diff.copy.as_ref(), " a\n b\n+c");
        assert!(!diff.numbered);
    }

    fn numbers(diff: &Diff) -> Vec<&str> {
        diff.lines
            .iter()
            .filter_map(|line| match line {
                DiffLine::Line { number, .. } => Some(number.as_ref()),
                DiffLine::Folded(_) => None,
            })
            .collect()
    }

    #[test]
    fn hunks_are_numbered_from_where_they_start() {
        let at = |line| ToolLocation {
            path: "a.rs".into(),
            line: Some(line),
        };
        let locations = [at(10), at(41)];
        let mut hunks = Hunks::new(&locations);
        let first = hunks.diff("a.rs".into(), Some("x\ny\n"), "x\n1\n2\ny\n");
        // The two lines added at 11 put the second hunk's old line 39 at 41.
        let second = hunks.diff("a.rs".into(), Some("p\nq\n"), "p\nr\n");
        assert_eq!(numbers(&first), ["10", "11", "12", "13"]);
        assert_eq!(numbers(&second), ["41", "40", "42"]);
        // A snippet with no line to start from goes unnumbered.
        let snippet = Hunks::new(&[]).diff("a.rs".into(), Some("x\n"), "y\n");
        assert!(!snippet.numbered && numbers(&snippet).iter().all(|n| n.is_empty()));
    }
}
