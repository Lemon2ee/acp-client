//! A tool call as its row keeps it: what the agent sent, made ready to draw once rather
//! than on every frame, and how long the call ran.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use acp_core::{ToolContent, ToolKind, ToolLocation, ToolStatus};
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

pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) kind: ToolKind,
    pub(crate) status: ToolStatus,
    /// The files it works on, as the agent names them.
    pub(crate) locations: Vec<ToolLocation>,
    pub(crate) content: Vec<Shown>,
    /// Lines its edits add and remove, over all its diffs.
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// When it came in, in a live turn. ACP sends no times, so a call a loaded session
    /// replays has none and shows no duration.
    started: Option<Instant>,
    /// How long it ran, from coming in to first finishing.
    pub(crate) took: Option<Duration>,
    /// The user has opened it to show its content.
    pub(crate) open: bool,
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
            added: 0,
            removed: 0,
            // One that arrives finished ran before it was shown, for no time we know.
            started: (live && !status.finished()).then(Instant::now),
            took: None,
            open: false,
        }
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
        if let Some(content) = content {
            self.set_content(content, cx);
        }
        if let Some(locations) = locations {
            self.locations = locations;
        }
    }

    /// How long it has been running, while it runs in a live turn.
    pub(crate) fn running_for(&self) -> Option<Duration> {
        match self.status {
            ToolStatus::InProgress if self.took.is_none() => {
                self.started.map(|started| started.elapsed())
            }
            _ => None,
        }
    }

    fn set_content(&mut self, content: Vec<ToolContent>, cx: &mut App) {
        self.content = content
            .into_iter()
            .filter_map(|content| match content {
                ToolContent::Text(text) => shown_text(text, cx),
                ToolContent::Diff { path, old, new } => {
                    Some(Shown::Diff(diff(path, old.as_deref().unwrap_or(""), &new)))
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

/// Text as it shows: one fenced code block as its lines, anything else as markdown.
/// Blank text shows nothing.
fn shown_text(text: String, cx: &mut App) -> Option<Shown> {
    if let Some(body) = fenced(&text) {
        let lines = code_lines(body);
        if lines.iter().all(|line| line.text.trim().is_empty()) {
            return None;
        }
        let copy = lines
            .iter()
            .map(|line| line.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        return Some(Shown::Code(Code {
            widest: widest(lines.iter().map(|line| line.text.as_ref())),
            lines: lines.into(),
            copy: copy.into(),
        }));
    }
    if text.trim().is_empty() {
        return None;
    }
    let view = cx.new(|cx| TextViewState::markdown(&text, cx));
    Some(Shown::Text(view, text.into()))
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
    let plain = |line: &str| CodeLine {
        number: None,
        text: line.replace('\t', TAB).into(),
    };
    let lines: Vec<&str> = body.lines().collect();
    let Some((last, rest)) = lines.split_last() else {
        return Vec::new();
    };
    let numbered: Option<Vec<CodeLine>> = rest.iter().map(|line| numbered_line(line)).collect();
    match numbered {
        Some(mut numbered) if !rest.is_empty() || numbered_line(last).is_some() => {
            numbered.push(numbered_line(last).unwrap_or_else(|| plain(last)));
            numbered
        }
        _ => lines.into_iter().map(plain).collect(),
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
fn diff(path: PathBuf, old: &str, new: &str) -> Diff {
    let text_diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_lines(old, new);
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
                let number = match change.tag() {
                    ChangeTag::Delete => change.old_index(),
                    _ => change.new_index(),
                };
                match change.tag() {
                    ChangeTag::Insert => added += 1,
                    ChangeTag::Delete => removed += 1,
                    ChangeTag::Equal => {}
                }
                let text = change.value().trim_end_matches(['\n', '\r']);
                lines.push(DiffLine::Line {
                    change: change.tag(),
                    number: number
                        .map_or_else(String::new, |n| (n + 1).to_string())
                        .into(),
                    text: text.replace('\t', TAB).into(),
                });
            }
        }
    }
    let copy = lines
        .iter()
        .map(|line| match line {
            DiffLine::Line { change, text, .. } => format!("{}{text}", change),
            DiffLine::Folded(count) => format!("⋯ {count} unchanged lines"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let widest = widest(lines.iter().map(|line| match line {
        DiffLine::Line { text, .. } => text.as_ref(),
        DiffLine::Folded(_) => "",
    }));
    Diff {
        path,
        lines: lines.into(),
        widest,
        added,
        removed,
        copy: copy.into(),
    }
}

/// The index of the line with the most characters. The text is drawn in a monospace
/// font, so that line draws widest, give or take wide characters.
fn widest<'a>(lines: impl Iterator<Item = &'a str>) -> usize {
    lines
        .map(|line| line.chars().count())
        .enumerate()
        .max_by_key(|(_, len)| *len)
        .map_or(0, |(ix, _)| ix)
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{DiffLine, code_lines, diff, fenced};
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
        let diff = diff("a.rs".into(), &old, &new);
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
        let diff = diff("new.rs".into(), "", "a\nb\n");
        assert_eq!((diff.added, diff.removed), (2, 0));
        assert_eq!(diff.copy.as_ref(), "+a\n+b");
    }
}
