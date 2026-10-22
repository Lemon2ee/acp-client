//! One conversation with an agent as the window holds it: the rows of its transcript,
//! its settings with the changes the agent hasn't answered yet, and how each of the
//! agent's events for it lands there.

mod pacing;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use acp_core::{
    PermissionRequest, SessionEvent, SessionKey, Setting, SettingKind, SettingValue, ToolStatus,
};
use gpui_kit::base::Easing;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::text::{TextViewMotion, TextViewState};
use gpui_kit::*;

use crate::chat_view::scroller::TranscriptScroll;
use crate::config::TitleSource;
use pacing::Pacer;

/// How much later each word of a streamed piece starts fading in than the one before,
/// as in gpui-component's own `TextView::stream_fade`.
const STREAM_FADE_STAGGER: Duration = Duration::from_millis(10);

pub(crate) enum Row {
    User(String),
    /// Markdown, grown chunk by chunk as the agent streams.
    Agent(Entity<TextViewState>),
    Thought(Entity<TextViewState>),
    Tool {
        id: String,
        title: String,
        status: ToolStatus,
    },
    Permission {
        request: PermissionRequest,
        /// Replaces the option buttons once the request is closed: how it was answered,
        /// or that it was cancelled.
        decided: Option<String>,
    },
    Notice(String),
    Error(String),
}

/// One conversation with an agent: its transcript and whether it takes a prompt.
pub(crate) struct Session {
    pub(crate) key: SessionKey,
    /// Index into `ChatView::agents` of the agent it runs on. A session never moves:
    /// ACP has no way to carry one over to another agent.
    pub(crate) agent: usize,
    /// Shown on the session's card and above its transcript: the first line of the first
    /// prompt, until the agent sends a title for the session or the user renames it.
    pub(crate) title: Option<String>,
    pub(crate) title_source: TitleSource,
    pub(crate) rows: Vec<Row>,
    pub(crate) scroller: Entity<TranscriptScroll>,
    /// Not ready for a prompt: a spare still being created, a turn is running, or the
    /// agent is gone. A session the agent hasn't created yet takes a prompt, which has
    /// the agent create it.
    pub(crate) busy: bool,
    /// Text typed here but not sent, kept while another session is on screen.
    pub(crate) draft: SharedString,
    /// What the agent lets the user choose for this session, such as its model; none
    /// until the agent has created it.
    pub(crate) settings: Vec<Setting>,
    /// The agent has created it and given its settings, if any.
    pub(crate) created: bool,
    /// Settings changed here whose changes the agent hasn't all answered, by setting id:
    /// switches, and every setting of a session the agent hasn't created yet, as no
    /// answer comes until it has.
    pub(crate) asked: HashMap<String, Asked>,
    /// The agent's id for it, once the agent has it, which keeps it for later runs.
    pub(crate) id: Option<String>,
    pub(crate) origin: Origin,
    /// In the sidebar's Archived list, out of the way.
    pub(crate) archived: bool,
    /// A turn prompted here is running, so the agent's text arrives as it is written: it
    /// fades in and new rows come in with motion. A loaded session's replay shows at once.
    live: bool,
    /// When each row added in a live turn came in, until its entrance has played. Kept
    /// here because the list drops a row's element state once it scrolls out of view.
    pub(crate) entering: HashMap<usize, Instant>,
    /// When the rows last changed, which the waiting line's delay counts from.
    pub(crate) changed: Instant,
    /// The agent's text in a live turn, held back to show at an even pace, and the
    /// transcript's events that came after it.
    pacer: Pacer,
}

/// Which run a session's agent session comes from.
pub(crate) enum Origin {
    /// This one: the agent creates it on its first prompt, or as a spare.
    New,
    /// An earlier one, and the agent hasn't been asked to load it yet: the folder it
    /// was created in, which loading it needs. It loads as it first shows.
    Saved(PathBuf),
    /// An earlier one, and the agent has been asked to load it: the folder it was
    /// created in.
    Loaded(PathBuf),
}

/// A setting changed here, until the agent has answered every change it was asked for.
pub(crate) struct Asked {
    /// The value last asked for, which the setting shows meanwhile.
    value: SettingValue,
    /// How many of its changes have no answer yet.
    out: u32,
}

impl Session {
    pub(crate) fn new(key: SessionKey, agent: usize, cx: &mut App) -> Self {
        Self {
            key,
            agent,
            title: None,
            title_source: TitleSource::Prompt,
            rows: Vec::new(),
            scroller: cx.new(|_| TranscriptScroll::new()),
            busy: false,
            draft: SharedString::default(),
            settings: Vec::new(),
            created: false,
            asked: HashMap::new(),
            id: None,
            origin: Origin::New,
            archived: false,
            live: false,
            entering: HashMap::new(),
            changed: Instant::now(),
            pacer: Pacer::default(),
        }
    }

    /// Starts or ends a live turn. Only a live turn shows the waiting line under the last
    /// row, so that row is measured again when it comes or goes.
    pub(crate) fn set_live(&mut self, live: bool, cx: &mut App) {
        if self.live == live {
            return;
        }
        let waiting = self.waiting();
        self.live = live;
        if self.waiting() != waiting {
            self.remeasure(self.rows.len() - 1, cx);
        }
    }

    /// Waiting on the agent in a live turn: nothing has come since the prompt, or since
    /// the last tool call finished.
    pub(crate) fn waiting(&self) -> bool {
        self.live
            && match self.rows.last() {
                Some(Row::User(_)) => true,
                Some(Row::Tool { status, .. }) => status.finished(),
                _ => false,
            }
    }

    /// A setting as it shows: one changed here shows the value last asked for until the
    /// agent has answered all its changes.
    pub(crate) fn shown(&self, setting: &Setting) -> Setting {
        let mut setting = setting.clone();
        if let Some(asked) = self.asked.get(&setting.id) {
            setting.value = asked.value.clone();
        }
        setting
    }

    /// The setting is to show `value` until the agent has answered the change just asked
    /// for, and any others still out.
    pub(crate) fn ask(&mut self, setting: String, value: SettingValue) {
        let asked = self.asked.entry(setting).or_insert(Asked {
            value: value.clone(),
            out: 0,
        });
        asked.value = value;
        asked.out += 1;
    }

    /// One of a setting's changes has its answer. The setting shows the agent's value
    /// again once all of its have, as an answer to an earlier change can arrive while a
    /// later one is still out.
    fn answered(&mut self, setting: &str) {
        if let Some(asked) = self.asked.get_mut(setting) {
            // A late answer to a change already given up on finds nothing to count.
            asked.out = asked.out.saturating_sub(1);
            if asked.out == 0 {
                self.asked.remove(setting);
            }
        }
    }

    /// Takes an event from the agent. In a live turn its text is held back to show at an
    /// even pace, and the transcript's other events wait behind it, keeping their order.
    pub(crate) fn apply(&mut self, event: SessionEvent, cx: &mut App) {
        if !self.live || !in_transcript(&event) || self.updates_shown_tool(&event) {
            return self.land(event, cx);
        }
        if self.pacer.after.is_empty() {
            self.take(event, cx);
        } else {
            self.pacer.after.push_back(event);
        }
    }

    /// An update to a tool call whose row shows already. It changes a row above any text
    /// held back, so it can't land out of order; held back, it would make the pacer take
    /// that text's row as finished while more of it may follow the update.
    fn updates_shown_tool(&self, event: &SessionEvent) -> bool {
        let SessionEvent::ToolCallUpdate { id, .. } = event else {
            return false;
        };
        self.rows
            .iter()
            .any(|row| matches!(row, Row::Tool { id: shown, .. } if shown == id))
    }

    /// Whether any text or event is held back, for `pace` to show.
    pub(crate) fn pacing(&self) -> bool {
        self.pacer.holds()
    }

    /// Shows the next piece of held-back text, `dt` after the last, and once all of it
    /// shows, lands what came after it.
    pub(crate) fn pace(&mut self, dt: Duration, cx: &mut App) -> Paced {
        let mut paced = Paced::default();
        if let Some(piece) = self.pacer.next(dt, Instant::now()) {
            self.show(piece, cx);
            paced.shown = true;
        }
        while !self.pacer.has_text() {
            let Some(event) = self.pacer.after.pop_front() else {
                break;
            };
            self.take(event, cx);
            paced.landed = true;
        }
        paced.holds = self.pacer.holds();
        paced
    }

    /// Shows all held-back text at once and lands what came after it, as when the user
    /// stops the turn or the agent is gone.
    pub(crate) fn flush(&mut self, cx: &mut App) {
        loop {
            if self.pacer.has_text() {
                let text = self.pacer.take_all();
                self.show(text, cx);
            }
            let Some(event) = self.pacer.after.pop_front() else {
                break;
            };
            self.take(event, cx);
        }
    }

    /// A transcript event in a live turn with no event before it still held back: text
    /// joins the held text, and anything else lands once no text is held.
    fn take(&mut self, event: SessionEvent, cx: &mut App) {
        let (thought, text) = match event {
            SessionEvent::AgentText(text) => (false, text),
            SessionEvent::AgentThought(text) => (true, text),
            event if self.pacer.has_text() => return self.pacer.after.push_back(event),
            event => return self.land(event, cx),
        };
        let streaming = self.pacer.opens.or(match self.rows.last() {
            Some(Row::Agent(_)) => Some(false),
            Some(Row::Thought(_)) => Some(true),
            _ => None,
        });
        if streaming != Some(thought) {
            // Text for another row waits until this one has shown all of its own.
            if self.pacer.has_text() {
                let event = if thought {
                    SessionEvent::AgentThought(text)
                } else {
                    SessionEvent::AgentText(text)
                };
                return self.pacer.after.push_back(event);
            }
            // Agents often open a block with a bare "\n\n" chunk; don't start a row with it.
            if text.trim().is_empty() {
                return;
            }
            self.pacer.start_row(thought);
        }
        self.pacer.arrived(&text, Instant::now());
    }

    /// Shows a piece of the held-back text: in the last row, or in a new one it opens.
    fn show(&mut self, piece: String, cx: &mut App) {
        if let Some(thought) = self.pacer.opens.take() {
            let view = cx.new(|cx| {
                // Pushed rather than set, so the row's first piece fades in too.
                let mut view = TextViewState::markdown("", cx).motion(stream_fade(cx));
                view.push_str(&piece, cx);
                view
            });
            let row = if thought {
                Row::Thought(view)
            } else {
                Row::Agent(view)
            };
            return self.push(row, cx);
        }
        if let Some(Row::Agent(view) | Row::Thought(view)) = self.rows.last() {
            view.update(cx, |view, cx| view.push_str(&piece, cx));
            self.remeasure(self.rows.len() - 1, cx);
        }
    }

    /// An event as it lands in the session, at once.
    fn land(&mut self, event: SessionEvent, cx: &mut App) {
        match event {
            SessionEvent::Created(id) => self.id = Some(id),
            SessionEvent::UserText(text) => self.append_user_text(&text, cx),
            SessionEvent::AgentText(text) => self.append_text(false, &text, cx),
            SessionEvent::AgentThought(text) => self.append_text(true, &text, cx),
            SessionEvent::ToolCall {
                id, title, status, ..
            } => self.push(Row::Tool { id, title, status }, cx),
            SessionEvent::ToolCallUpdate {
                id, title, status, ..
            } => {
                let found =
                    self.rows
                        .iter_mut()
                        .enumerate()
                        .rev()
                        .find_map(|(ix, row)| match row {
                            Row::Tool {
                                id: row_id,
                                title: row_title,
                                status: row_status,
                            } if *row_id == id => {
                                if let Some(title) = title.clone() {
                                    *row_title = title;
                                }
                                if let Some(status) = status {
                                    *row_status = status;
                                }
                                Some(ix)
                            }
                            _ => None,
                        });
                if let Some(ix) = found {
                    self.remeasure(ix, cx);
                }
            }
            SessionEvent::Plan(entries) => {
                self.push(Row::Notice(format!("plan\n{}", entries.join("\n"))), cx)
            }
            SessionEvent::Permission(request) => self.push(
                Row::Permission {
                    request,
                    decided: None,
                },
                cx,
            ),
            // The turn's last row still comes in as the turn's others did.
            SessionEvent::TurnEnded { stop_reason } => {
                self.busy = false;
                self.cancel_open_permissions(cx);
                if stop_reason != "EndTurn" {
                    self.push(Row::Notice(format!("turn ended: {stop_reason}")), cx);
                }
                self.set_live(false, cx);
            }
            SessionEvent::TurnFailed(message) => {
                self.busy = false;
                self.cancel_open_permissions(cx);
                self.push(Row::Error(message), cx);
                self.set_live(false, cx);
            }
            SessionEvent::Settings(settings) => {
                self.settings = settings;
                self.created = true;
            }
            SessionEvent::SettingChanged { setting, settings } => {
                self.settings = settings;
                self.answered(&setting);
            }
            SessionEvent::ModeChanged(mode) => {
                for setting in &mut self.settings {
                    if setting.kind == SettingKind::Mode {
                        setting.value = SettingValue::Choice(mode.clone());
                    }
                }
            }
            SessionEvent::SettingFailed { setting, message } => {
                self.answered(&setting);
                self.push_error(message, cx)
            }
            SessionEvent::Title(title) => {
                if self.title_source == TitleSource::User || self.echoes_prompt(&title) {
                    return;
                }
                self.title = Some(title);
                self.title_source = TitleSource::Agent;
            }
        }
    }

    /// Whether the title is only the first prompt run onto one line: what the adapters
    /// send when they couldn't have a title made (Claude's), or until it is made (Codex's).
    /// The first line shows already and reads better.
    fn echoes_prompt(&self, title: &str) -> bool {
        let prompt = self.rows.iter().find_map(|row| match row {
            Row::User(text) => Some(text),
            _ => None,
        });
        prompt.is_some_and(|prompt| echoes(prompt, title))
    }

    /// acp-core stops taking answers to a turn's permission requests once the turn is
    /// stopped or over, so the cards still open show as cancelled instead of offering options.
    pub(crate) fn cancel_open_permissions(&mut self, cx: &mut App) {
        for ix in 0..self.rows.len() {
            if let Row::Permission {
                decided: decided @ None,
                ..
            } = &mut self.rows[ix]
            {
                *decided = Some("cancelled".into());
                self.remeasure(ix, cx);
            }
        }
    }

    pub(crate) fn answer(&mut self, ix: usize, choice: Option<String>, cx: &mut App) {
        if let Some(Row::Permission { request, decided }) = self.rows.get_mut(ix) {
            let label = choice
                .as_ref()
                .and_then(|id| request.options.iter().find(|option| option.id == *id))
                .map_or_else(|| "rejected".to_string(), |option| option.name.clone());
            // The turn may have been stopped or ended since the card was drawn.
            *decided = Some(if request.answer(choice) {
                format!("answered: {label}")
            } else {
                "cancelled".into()
            });
            self.remeasure(ix, cx);
        }
    }

    /// Streams arrive as small chunks; grow the last row instead of adding one per chunk.
    /// Only text outside a live turn comes here, such as a loaded session's replay.
    fn append_text(&mut self, thought: bool, text: &str, cx: &mut App) {
        match self.rows.last() {
            Some(Row::Agent(existing)) if !thought => {
                existing.update(cx, |view, cx| view.push_str(text, cx))
            }
            Some(Row::Thought(existing)) if thought => {
                existing.update(cx, |view, cx| view.push_str(text, cx))
            }
            // Agents often open a block with a bare "\n\n" chunk; don't start a row with it.
            _ if text.trim().is_empty() => return,
            _ => {
                let view = cx.new(|cx| TextViewState::markdown(text, cx));
                let row = if thought {
                    Row::Thought(view)
                } else {
                    Row::Agent(view)
                };
                return self.push(row, cx);
            }
        }
        self.remeasure(self.rows.len() - 1, cx);
    }

    /// The user's own messages come back in parts as the agent replays a session it
    /// loads, each part of a message after the one before.
    fn append_user_text(&mut self, text: &str, cx: &mut App) {
        match self.rows.last_mut() {
            Some(Row::User(message)) => message.push_str(text),
            _ => return self.push(Row::User(text.to_string()), cx),
        }
        self.remeasure(self.rows.len() - 1, cx);
    }

    /// An error from outside the turn's own events, such as the connection's or a setting
    /// change's. What the turn has sent shows first, all of it, so the error comes after
    /// it and no held-back text is left for a row that is no longer the last.
    pub(crate) fn push_error(&mut self, message: String, cx: &mut App) {
        self.flush(cx);
        self.push(Row::Error(message), cx);
    }

    pub(crate) fn push(&mut self, row: Row, cx: &mut App) {
        let now = Instant::now();
        if self.live {
            self.entering.insert(self.rows.len(), now);
        }
        self.changed = now;
        self.rows.push(row);
        self.scroller.update(cx, |scroller, _| scroller.push());
    }

    fn remeasure(&mut self, ix: usize, cx: &mut App) {
        self.changed = Instant::now();
        self.scroller
            .update(cx, |scroller, _| scroller.remeasure(ix));
    }
}

/// What one `Session::pace` step did.
#[derive(Default)]
pub(crate) struct Paced {
    /// A piece of held-back text showed.
    pub(crate) shown: bool,
    /// An event that waited behind the text landed.
    pub(crate) landed: bool,
    /// Text or events are still held back.
    pub(crate) holds: bool,
}

/// Events that land in the transcript, which keep their order behind held-back text.
/// The others concern settings and land at once.
fn in_transcript(event: &SessionEvent) -> bool {
    !matches!(
        event,
        SessionEvent::Created(_)
            | SessionEvent::Settings(_)
            | SessionEvent::SettingChanged { .. }
            | SessionEvent::ModeChanged(_)
            | SessionEvent::SettingFailed { .. }
            | SessionEvent::Title(_)
    )
}

/// Whether `title` is `prompt` with its whitespace run together, whole or cut short with
/// an ellipsis, as both adapters cut a title at 256 characters.
fn echoes(prompt: &str, title: &str) -> bool {
    let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    match title.strip_suffix('…') {
        Some(head) => prompt.len() > head.len() && prompt.starts_with(head.trim_end()),
        None => prompt == title,
    }
}

/// The agent's text fading in as it streams: each piece over the theme's slow duration,
/// its words one after another, as `TextView::stream_fade` does. A piece too large to
/// light up within one fade, such as one after a stall, fades as a whole. gpui-component
/// draws it at once while macOS's Reduce motion is on.
fn stream_fade(cx: &App) -> TextViewMotion {
    TextViewMotion::default()
        .with_stream_fade(cx.theme().motion_tokens().duration_slow)
        .with_stream_fade_stagger(STREAM_FADE_STAGGER)
        .with_stream_fade_easing(Easing::EaseOut)
}

/// A selectable row's element id, which carries the session so a row of one session
/// never inherits the selection state of another session's row at the same index.
pub(crate) fn row_id(kind: &str, key: SessionKey, ix: usize) -> ElementId {
    ElementId::Name(format!("{kind}-{}-{ix}", key.0).into())
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::echoes;

    #[test]
    fn a_prompt_run_onto_one_line_echoes_it() {
        let prompt = "Fix the build\n\nIt fails on  macOS";
        assert!(echoes(prompt, "Fix the build It fails on macOS"));
        assert!(echoes(prompt, "Fix the build It fails…"));
        assert!(echoes(
            "先列出文件\n再读 Cargo.toml",
            "先列出文件 再读 Cargo.toml"
        ));
    }

    #[test]
    fn a_made_title_doesnt_echo_the_prompt() {
        let prompt = "Fix the build\n\nIt fails on macOS";
        assert!(!echoes(prompt, "Fix macOS build failure"));
        assert!(!echoes(prompt, "Fix the build"));
        // An ellipsis on the whole prompt cuts nothing off it.
        assert!(!echoes("Fix the build", "Fix the build…"));
    }
}
