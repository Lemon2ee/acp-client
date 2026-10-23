//! A session's rows as kept on disk between runs, so that a session from an earlier run
//! shows its transcript at once while the agent loads it: the form each row is written in,
//! when rows are due to be written, and how the agent's replay takes the kept rows' place
//! once the load is done.

use std::time::{Duration, Instant};

use acp_core::{SessionEvent, ToolContent, ToolKind, ToolLocation, ToolStatus};
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices_deadline};

use super::{Markdown, Row, Session, ToolCall};
use crate::config::StuffAttachment;

/// How long lining the kept rows up with a replay may take before it settles for a coarser
/// match, which only draws more rows again, so a long transcript can't stall the window.
const LINE_UP_TIMEOUT: Duration = Duration::from_millis(50);

/// A row of a transcript as kept on disk, a line of JSON each.
#[derive(Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "lowercase")]
pub(crate) enum KeptRow {
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<StuffAttachment>,
    },
    Agent {
        text: String,
    },
    Thought {
        text: String,
    },
    Tool {
        id: String,
        title: String,
        kind: ToolKind,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        locations: Vec<ToolLocation>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolContent>,
        /// How long it ran, in milliseconds, for a call a live turn timed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        took_ms: Option<u64>,
    },
    Notice {
        text: String,
    },
    Error {
        text: String,
    },
}

impl KeptRow {
    /// What a prompt was sent with, which its row shows as tiles.
    pub(crate) fn attachments(&self) -> &[StuffAttachment] {
        match self {
            KeptRow::User { attachments, .. } => attachments,
            _ => &[],
        }
    }
}

/// The agent's replay of a session whose kept rows stand in for its transcript.
pub(super) struct Replay {
    rows: Vec<Row>,
    /// `Session::after_turn` for the replay's rows.
    after_turn: bool,
    /// How many of the session's rows are the kept ones; any after them came while they
    /// stood in, such as an error.
    kept: usize,
}

impl Row {
    /// The row as it is kept on disk. What it held only to be kept goes into it.
    fn keep(&mut self) -> KeptRow {
        match self {
            Row::User { text, attachments } => KeptRow::User {
                text: text.clone(),
                attachments: attachments.clone(),
            },
            Row::Agent(text) => KeptRow::Agent {
                text: text.source.take().unwrap_or_default(),
            },
            Row::Thought(text) => KeptRow::Thought {
                text: text.source.take().unwrap_or_default(),
            },
            Row::Tool(call) => KeptRow::Tool {
                id: call.id.clone(),
                title: call.title.clone(),
                kind: call.kind,
                status: call.status,
                locations: call.locations.clone(),
                content: call.sent_content.take().unwrap_or_default(),
                took_ms: call.took.map(|took| took.as_millis() as u64),
            },
            Row::Notice(text) => KeptRow::Notice { text: text.clone() },
            Row::Error(text) => KeptRow::Error { text: text.clone() },
        }
    }

    /// Drops what it held only to be kept, once it is on disk as it stands.
    fn forget_kept(&mut self) {
        match self {
            Row::Agent(text) | Row::Thought(text) => text.source = None,
            Row::Tool(call) => call.sent_content = None,
            Row::User { .. } | Row::Notice(_) | Row::Error(_) => {}
        }
    }

    /// A row kept from an earlier run, as the agent's replay of it would make it.
    fn from_kept(kept: KeptRow, cx: &mut App) -> Row {
        let markdown = |text: String, cx: &mut App| {
            let view = cx.new(|cx| TextViewState::markdown(&text, cx));
            Markdown::new(view, text)
        };
        match kept {
            KeptRow::User { text, attachments } => Row::User { text, attachments },
            KeptRow::Agent { text } => Row::Agent(markdown(text, cx)),
            KeptRow::Thought { text } => Row::Thought(markdown(text, cx)),
            KeptRow::Tool {
                id,
                title,
                kind,
                status,
                locations,
                content,
                took_ms,
            } => {
                let mut call = ToolCall::new(id, title, kind, status, false);
                call.update(None, None, None, Some(content), Some(locations), cx);
                call.took = took_ms.map(Duration::from_millis);
                Row::Tool(call)
            }
            KeptRow::Notice { text } => Row::Notice(text),
            KeptRow::Error { text } => Row::Error(text),
        }
    }
}

impl Session {
    /// Shows the rows kept on disk from an earlier run while the agent loads the session.
    /// Its replay lands out of sight meanwhile, and takes their place once the load is done
    /// (see `loaded`), so the two never show one after the other.
    pub(crate) fn stand_in(&mut self, kept: Vec<KeptRow>, cx: &mut App) {
        if kept.is_empty() || !self.rows.is_empty() {
            return;
        }
        self.rows = kept
            .into_iter()
            .map(|row| Row::from_kept(row, cx))
            .collect();
        let count = self.rows.len();
        self.written = count;
        // Text the agent sends after the load is a message of its own.
        self.after_turn = true;
        self.replay = Some(Replay {
            rows: Vec::new(),
            after_turn: false,
            kept: count,
        });
        self.remeasure_from(0, cx);
    }

    /// Lands an event of the agent's replay among the replay's rows, out of sight. Its
    /// calls aren't timed and its permission requests can't be answered, as any it lands
    /// outside a live turn.
    pub(super) fn land_replayed(&mut self, event: SessionEvent, cx: &mut App) {
        let Some(mut replay) = self.replay.take() else {
            return self.land(event, cx);
        };
        std::mem::swap(&mut self.rows, &mut replay.rows);
        std::mem::swap(&mut self.after_turn, &mut replay.after_turn);
        let live = std::mem::replace(&mut self.live, false);
        self.offstage = true;
        self.land(event, cx);
        self.offstage = false;
        self.live = live;
        std::mem::swap(&mut self.rows, &mut replay.rows);
        std::mem::swap(&mut self.after_turn, &mut replay.after_turn);
        self.replay = Some(replay);
    }

    /// The agent has loaded the session and replayed it. What it replayed takes the place
    /// of the kept rows that stood in, if any did, and is kept as the session's transcript.
    pub(super) fn loaded(&mut self, cx: &mut App) {
        match self.replay.take() {
            Some(replay) => self.take_replay(replay, cx),
            // The replay landed on screen: nothing was kept to stand in for it.
            None => {
                self.written = self.rows.len();
                self.rewrite = !self.rows.is_empty();
            }
        }
        self.set_loading(false, cx);
    }

    fn take_replay(&mut self, replay: Replay, cx: &mut App) {
        let added = self.rows.split_off(replay.kept.min(self.rows.len()));
        let kept = std::mem::take(&mut self.rows);
        // An agent that replays nothing has nothing better to show.
        let merged = if replay.rows.is_empty() {
            Merged::unchanged(kept)
        } else {
            merge(kept, replay.rows)
        };
        self.rows = merged.rows;
        self.written = self.rows.len();
        if merged.changed {
            self.rewrite = true;
        } else {
            self.rows.iter_mut().for_each(Row::forget_kept);
        }
        self.rows.extend(added);
        // Rows from `first` on may be other rows than they were.
        self.groups_set.retain(|&start, _| start < merged.first);
        self.entering.retain(|&ix, _| ix < merged.first);
        self.remeasure_from(merged.first, cx);
    }

    /// Has the list take the rows from `from` on as new ones.
    fn remeasure_from(&mut self, from: usize, cx: &mut App) {
        let count = self.rows.len() - from;
        self.changed = Instant::now();
        self.scroller
            .update(cx, |scroller, _| scroller.replace(from, count));
    }

    /// The whole transcript, to write in place of the one on disk, once what the agent
    /// replayed has taken the kept rows' place and differs from them.
    pub(crate) fn rewrite(&mut self) -> Option<Vec<KeptRow>> {
        if self.replay.is_some() || !std::mem::take(&mut self.rewrite) {
            return None;
        }
        let written = self.written.min(self.rows.len());
        Some(self.rows[..written].iter_mut().map(Row::keep).collect())
    }

    /// The rows added since the transcript was last written, to add to it, once a turn has
    /// ended. They are the turn's and any that came between turns.
    pub(crate) fn new_rows(&mut self) -> Option<Vec<KeptRow>> {
        if self.replay.is_some() || !std::mem::take(&mut self.turn_ended) {
            return None;
        }
        let written = self.written.min(self.rows.len());
        self.written = self.rows.len();
        Some(self.rows[written..].iter_mut().map(Row::keep).collect())
    }
}

/// The rows a replay leaves.
struct Merged {
    rows: Vec<Row>,
    /// The first of them that isn't the kept row that was in its place.
    first: usize,
    /// They aren't the kept rows as they were.
    changed: bool,
}

impl Merged {
    fn unchanged(rows: Vec<Row>) -> Self {
        Self {
            first: rows.len(),
            rows,
            changed: false,
        }
    }
}

/// The replayed rows, with the kept row in place of each that lines up with it. That keeps
/// what only a live turn showed, such as the tiles a prompt was sent with and how long each
/// call took, and spares drawing again what is already drawn. Notices and errors are this
/// client's own, which no replay has, so they stay where they were.
fn merge(kept: Vec<Row>, replayed: Vec<Row>) -> Merged {
    let picks = {
        let old: Vec<Key> = kept.iter().map(Key::of).collect();
        let new: Vec<Key> = replayed.iter().map(Key::of).collect();
        line_up(&old, &new)
    };
    let count = kept.len();
    let mut kept: Vec<Option<Row>> = kept.into_iter().map(Some).collect();
    let mut replayed: Vec<Option<Row>> = replayed.into_iter().map(Some).collect();
    let mut rows = Vec::with_capacity(picks.len());
    let mut first = None;
    for pick in picks {
        let row = match pick {
            Pick::Kept(ix) => kept[ix].take(),
            Pick::Replayed(ix) => replayed[ix].take(),
        };
        let Some(row) = row else { continue };
        if first.is_none() && pick != Pick::Kept(rows.len()) {
            first = Some(rows.len());
        }
        rows.push(row);
    }
    let first = first.unwrap_or(rows.len());
    Merged {
        changed: first < rows.len() || rows.len() != count,
        first,
        rows,
    }
}

/// What a row is, for lining the kept rows up with the replayed ones.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Key<'a> {
    /// A prompt matches any prompt: the replay gives the tiles sent with one as text.
    User,
    Agent(&'a str),
    Thought(&'a str),
    /// A tool call matches by its id and where it stands (`ToolStatus` as a number). An
    /// adapter may replay other content than it sent live, as Claude's does an edit.
    Tool(&'a str, u8),
    Notice(&'a str),
    Error(&'a str),
}

impl<'a> Key<'a> {
    fn of(row: &'a Row) -> Self {
        let source = |text: &'a Markdown| text.source.as_deref().unwrap_or_default();
        match row {
            Row::User { .. } => Key::User,
            Row::Agent(text) => Key::Agent(source(text)),
            Row::Thought(text) => Key::Thought(source(text)),
            Row::Tool(call) => Key::Tool(&call.id, call.status as u8),
            Row::Notice(text) => Key::Notice(text),
            Row::Error(text) => Key::Error(text),
        }
    }

    /// A kept row like this stays where it was without a replayed row to line up with.
    fn client_only(&self) -> bool {
        matches!(self, Key::Notice(_) | Key::Error(_))
    }
}

/// Where each of the merged rows comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
    Kept(usize),
    Replayed(usize),
}

/// The rows a replay leaves, in order: each replayed row, or the kept row it lines up
/// with, and the kept rows only this client shows.
fn line_up(kept: &[Key], replayed: &[Key]) -> Vec<Pick> {
    let mut picks = Vec::with_capacity(replayed.len());
    let deadline = Instant::now() + LINE_UP_TIMEOUT;
    for op in capture_diff_slices_deadline(Algorithm::Myers, kept, replayed, Some(deadline)) {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            picks.extend(old.map(Pick::Kept));
            continue;
        }
        picks.extend(new.map(Pick::Replayed));
        picks.extend(old.filter(|&ix| kept[ix].client_only()).map(Pick::Kept));
    }
    picks
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{KeptRow, Key, Pick, line_up};
    use acp_core::{ToolContent, ToolKind, ToolStatus};

    use Pick::{Kept, Replayed};

    const DONE: ToolStatus = ToolStatus::Completed;

    #[test]
    fn a_replay_like_the_kept_rows_keeps_them_all() {
        let rows = [
            Key::User,
            Key::Thought("hm"),
            Key::Tool("a", DONE as u8),
            Key::Agent("done"),
        ];
        assert_eq!(line_up(&rows, &rows), [Kept(0), Kept(1), Kept(2), Kept(3)]);
    }

    #[test]
    fn notices_and_errors_stay_where_they_were() {
        let kept = [
            Key::User,
            Key::Agent("a"),
            Key::Notice("turn ended: Cancelled"),
            Key::User,
            Key::Error("rate limited"),
            Key::User,
            Key::Agent("b"),
        ];
        let replayed = [
            Key::User,
            Key::Agent("a"),
            Key::User,
            Key::User,
            Key::Agent("b"),
        ];
        assert_eq!(
            line_up(&kept, &replayed),
            [
                Kept(0),
                Kept(1),
                Kept(2),
                Kept(3),
                Kept(4),
                Kept(5),
                Kept(6)
            ]
        );
    }

    #[test]
    fn what_the_agent_replays_differently_replaces_the_kept_rows() {
        let kept = [
            Key::User,
            Key::Agent("old"),
            Key::Tool("a", ToolStatus::InProgress as u8),
            Key::User,
        ];
        let replayed = [
            Key::User,
            Key::Agent("new"),
            Key::Tool("a", ToolStatus::Failed as u8),
            Key::User,
            Key::Agent("more"),
        ];
        assert_eq!(
            line_up(&kept, &replayed),
            [Kept(0), Replayed(1), Replayed(2), Kept(3), Replayed(4)]
        );
    }

    #[test]
    fn a_tool_row_reads_back_as_written() {
        let row = KeptRow::Tool {
            id: "toolu_1".into(),
            title: "Read a.rs".into(),
            kind: ToolKind::Read,
            status: DONE,
            locations: Vec::new(),
            content: vec![ToolContent::Diff {
                path: "a.rs".into(),
                old: None,
                new: "fn main() {}\n".into(),
            }],
            took_ms: Some(1_250),
        };
        let line = serde_json::to_string(&row).unwrap();
        assert!(line.starts_with(r#"{"row":"tool","#) && !line.contains("locations"));
        let KeptRow::Tool {
            content, took_ms, ..
        } = serde_json::from_str(&line).unwrap()
        else {
            panic!("a tool row reads back as one");
        };
        assert_eq!(took_ms, Some(1_250));
        assert!(matches!(
            &content[..],
            [ToolContent::Diff { old: None, .. }]
        ));
    }
}
