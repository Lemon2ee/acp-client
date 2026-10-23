//! One conversation with an agent as the window holds it: the rows of its transcript,
//! its settings with the changes the agent hasn't answered yet, and how each of the
//! agent's events for it lands there.

mod pacing;
mod tool_call;

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acp_core::{
    SessionEvent, SessionKey, Setting, SettingKind, SettingValue, ToolKind, ToolStatus,
};
use gpui_kit::base::Easing;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::text::{TextViewMotion, TextViewState};
use gpui_kit::*;

use crate::chat_view::scroller::TranscriptScroll;
use crate::config::TitleSource;
use crate::debug::switches;
use pacing::Pacer;
pub(crate) use tool_call::{Answer, Ask, Code, Diff, DiffLine, Shown, ToolCall, folded_text};

/// How much later each word of a streamed piece starts fading in than the one before,
/// as in gpui-component's own `TextView::stream_fade`.
const STREAM_FADE_STAGGER: Duration = Duration::from_millis(10);

pub(crate) enum Row {
    User(String),
    /// Markdown, grown chunk by chunk as the agent streams.
    Agent(Entity<TextViewState>),
    Thought(Entity<TextViewState>),
    /// A tool call, with the agent's request for permission to run it if it sent one.
    Tool(ToolCall),
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
    /// A setting changed here that goes to the agent once the user stops changing it, as
    /// the effort meter's levels do: a scroll from Low to Max asks for one change, not
    /// four.
    pub(crate) held: Option<Held>,
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
    /// Groups of tool calls the user opened or closed, by their first row, which keep
    /// that rather than opening and closing with the turn.
    groups_set: HashMap<usize, bool>,
    /// A turn ended and no text has come since. Text the agent sends between turns, as
    /// acp-core passes it on, is a message of its own rather than more of the turn's last
    /// reply.
    after_turn: bool,
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

/// A setting's value that shows at once and is asked for after a pause (see
/// `ChatView::hold_setting`).
pub(crate) struct Held {
    pub(crate) setting: String,
    pub(crate) value: SettingValue,
    /// Asks for it once the pause is over. A newer value drops it, and its own pause.
    pub(crate) _send: Task<()>,
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
            held: None,
            id: None,
            origin: Origin::New,
            archived: false,
            live: false,
            entering: HashMap::new(),
            changed: Instant::now(),
            pacer: Pacer::default(),
            groups_set: HashMap::new(),
            after_turn: false,
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
        // A group of tool calls the turn ended on closes.
        if !live {
            self.remeasure_tail_group(cx);
        }
    }

    /// A turn prompted here is running.
    pub(crate) fn is_live(&self) -> bool {
        self.live
    }

    /// Waiting on the agent in a live turn: nothing has come since the prompt, or since
    /// the last tool call finished.
    pub(crate) fn waiting(&self) -> bool {
        self.live
            && match self.rows.last() {
                Some(Row::User(_)) => true,
                Some(Row::Tool(call)) => call.status.finished(),
                _ => false,
            }
    }

    /// A setting as it shows: one changed here shows the value last asked for until the
    /// agent has answered all its changes, or the value held to ask for (see `held`).
    pub(crate) fn shown(&self, setting: &Setting) -> Setting {
        let mut setting = setting.clone();
        if let Some(asked) = self.asked.get(&setting.id) {
            setting.value = asked.value.clone();
        }
        if let Some(held) = self.held.as_ref().filter(|held| held.setting == setting.id) {
            setting.value = held.value.clone();
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
        if !switches().pace {
            // Shows as it arrives, still in order and fading in.
            self.flush(cx);
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
            .any(|row| matches!(row, Row::Tool(call) if call.id == *id))
    }

    /// How much of the streaming row's text shows, and how much has arrived, in bytes.
    pub(crate) fn paced_text(&self) -> (usize, usize) {
        self.pacer.progress()
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
                id,
                title,
                kind,
                status,
                content,
                locations,
            } => {
                let mut call = ToolCall::new(id, title, kind, status, self.live);
                call.update(None, None, None, Some(content), Some(locations), cx);
                self.push(Row::Tool(call), cx)
            }
            SessionEvent::ToolCallUpdate {
                id,
                title,
                kind,
                status,
                content,
                locations,
            } => {
                // A call the agent never sent comes first in a permission request; acp-core
                // sends its fields as an update just before.
                let ix = self.tool_call_row(&id).unwrap_or_else(|| {
                    let title = title.clone().unwrap_or_else(|| "Tool call".into());
                    let call = ToolCall::new(
                        id,
                        title,
                        kind.unwrap_or(ToolKind::Other),
                        status.unwrap_or(ToolStatus::Pending),
                        self.live,
                    );
                    self.push(Row::Tool(call), cx);
                    self.rows.len() - 1
                });
                if let Row::Tool(call) = &mut self.rows[ix] {
                    call.update(title, kind, status, content, locations, cx);
                }
                self.remeasure(ix, cx);
            }
            SessionEvent::Plan(entries) => {
                self.push(Row::Notice(format!("plan\n{}", entries.join("\n"))), cx)
            }
            SessionEvent::Permission(request) => {
                let ix = self.tool_call_row(&request.tool_call).unwrap_or_else(|| {
                    let call = ToolCall::new(
                        request.tool_call.clone(),
                        request.title.clone(),
                        ToolKind::Other,
                        ToolStatus::Pending,
                        self.live,
                    );
                    self.push(Row::Tool(call), cx);
                    self.rows.len() - 1
                });
                if let Row::Tool(call) = &mut self.rows[ix] {
                    call.ask(request, cx);
                    // acp-core answers a request that comes between turns as cancelled,
                    // having no turn to show it in, so it shows as one.
                    if !self.live
                        && let Some(ask) = call.ask.as_mut()
                    {
                        ask.answer = Answer::Cancelled;
                    }
                }
                // The call's group opens for the card.
                self.remeasure_group(ix, cx);
            }
            // The turn's last row still comes in as the turn's others did.
            SessionEvent::TurnEnded { stop_reason } => {
                self.busy = false;
                self.after_turn = true;
                self.cancel_open_permissions(cx);
                if stop_reason != "EndTurn" {
                    self.push(Row::Notice(format!("turn ended: {stop_reason}")), cx);
                }
                self.set_live(false, cx);
            }
            SessionEvent::TurnFailed(message) => {
                self.busy = false;
                self.after_turn = true;
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
            // Nothing in the window shows these yet.
            SessionEvent::ContextUsage(_)
            | SessionEvent::TurnUsage(_)
            | SessionEvent::ContextReport(_)
            | SessionEvent::ContextReadable(_) => {}
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

    /// Opens or closes the tool call at `ix`, to show what it produced or the change it
    /// makes.
    pub(crate) fn toggle_tool_call(&mut self, ix: usize, cx: &mut App) {
        if let Some(Row::Tool(call)) = self.rows.get_mut(ix) {
            call.open = !call.open;
            self.remeasure(ix, cx);
        }
    }

    /// The run of tool calls with no other row between them that the one at `ix` is in.
    pub(crate) fn tool_group(&self, ix: usize) -> Range<usize> {
        let is_tool = |row: &Row| matches!(row, Row::Tool(_));
        let start = self.rows[..ix]
            .iter()
            .rposition(|row| !is_tool(row))
            .map_or(0, |before| before + 1);
        let end = self.rows[ix..]
            .iter()
            .position(|row| !is_tool(row))
            .map_or(self.rows.len(), |after| ix + after);
        start..end
    }

    /// Whether a group of tool calls shows its calls: while one of them asks for
    /// permission; as the user left it; or else while it is the latest thing in a live
    /// turn, so the calls show as they run and fold once the agent writes again.
    pub(crate) fn group_open(&self, group: &Range<usize>) -> bool {
        let asking = self.rows[group.clone()]
            .iter()
            .any(|row| matches!(row, Row::Tool(call) if call.asking()));
        asking
            || self
                .groups_set
                .get(&group.start)
                .copied()
                .unwrap_or(self.live && group.end == self.rows.len())
    }

    /// Opens or closes the group of tool calls that starts at `first`, for good.
    pub(crate) fn toggle_tool_group(&mut self, first: usize, cx: &mut App) {
        let group = self.tool_group(first);
        let open = !self.group_open(&group);
        self.groups_set.insert(group.start, open);
        self.remeasure_group(first, cx);
    }

    /// Has the group of tool calls the one at `ix` is in measured again, as it opens,
    /// closes or changes its header.
    fn remeasure_group(&mut self, ix: usize, cx: &mut App) {
        for ix in self.tool_group(ix) {
            self.remeasure(ix, cx);
        }
    }

    /// Has the group of tool calls at the end measured again, as it is about to close by
    /// itself: rows of it out of view would otherwise keep their open height.
    fn remeasure_tail_group(&mut self, cx: &mut App) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            return;
        };
        if !matches!(self.rows[last], Row::Tool(_)) {
            return;
        }
        if !self.groups_set.contains_key(&self.tool_group(last).start) {
            self.remeasure_group(last, cx);
        }
    }

    /// The folder the agent works in for it, where it comes from an earlier run; a new
    /// one works in the window's.
    pub(crate) fn folder(&self) -> Option<&Path> {
        match &self.origin {
            Origin::Saved(folder) | Origin::Loaded(folder) => Some(folder),
            Origin::New => None,
        }
    }

    /// acp-core stops taking answers to a turn's permission requests once the turn is
    /// stopped or over, so the cards still open show as cancelled instead of offering options.
    pub(crate) fn cancel_open_permissions(&mut self, cx: &mut App) {
        for ix in 0..self.rows.len() {
            if let Row::Tool(call) = &mut self.rows[ix]
                && let Some(ask) = call.ask.as_mut().filter(|ask| ask.answer == Answer::Open)
            {
                ask.answer = Answer::Cancelled;
                self.remeasure_group(ix, cx);
            }
        }
    }

    /// Answers the permission request on the tool call at `ix` with an option's id, or
    /// `None` to reject it.
    pub(crate) fn answer(&mut self, ix: usize, choice: Option<String>, cx: &mut App) {
        let Some(Row::Tool(call)) = self.rows.get_mut(ix) else {
            return;
        };
        let Some(ask) = call.ask.as_mut().filter(|ask| ask.answer == Answer::Open) else {
            return;
        };
        let picked = choice
            .as_ref()
            .and_then(|id| ask.request.options.iter().find(|option| option.id == *id));
        let (allowed, always) =
            picked.map_or((false, false), |option| (option.allow, option.always));
        // The turn may have been stopped or ended since the card was drawn.
        ask.answer = match ask.request.answer(choice) {
            false => Answer::Cancelled,
            true if allowed => Answer::Allowed,
            true => Answer::Rejected,
        };
        ask.always = always && ask.answer != Answer::Cancelled;
        // It runs from now, not from when it came in to ask: the wait was the user's.
        if ask.answer == Answer::Allowed && call.started.is_some() {
            call.started = Some(Instant::now());
        }
        self.remeasure_group(ix, cx);
    }

    /// The row of the latest tool call with this id.
    fn tool_call_row(&self, id: &str) -> Option<usize> {
        self.rows
            .iter()
            .rposition(|row| matches!(row, Row::Tool(call) if call.id == id))
    }

    /// Streams arrive as small chunks; grow the last row instead of adding one per chunk.
    /// Only text outside a live turn comes here: a loaded session's replay, or what the
    /// agent sends between turns.
    fn append_text(&mut self, thought: bool, text: &str, cx: &mut App) {
        let joins = !self.after_turn;
        match self.rows.last() {
            Some(Row::Agent(existing)) if !thought && joins => {
                existing.update(cx, |view, cx| view.push_str(text, cx))
            }
            Some(Row::Thought(existing)) if thought && joins => {
                existing.update(cx, |view, cx| view.push_str(text, cx))
            }
            // Agents often open a block with a bare "\n\n" chunk; don't start a row with it.
            _ if text.trim().is_empty() => return,
            _ => {
                self.after_turn = false;
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
        // A group of tool calls closes once the agent goes on to something else.
        if !matches!(row, Row::Tool(_)) {
            self.remeasure_tail_group(cx);
        }
        let now = Instant::now();
        if self.live && switches().enter {
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
/// The others concern settings or the context window and land at once.
fn in_transcript(event: &SessionEvent) -> bool {
    !matches!(
        event,
        SessionEvent::Created(_)
            | SessionEvent::Settings(_)
            | SessionEvent::SettingChanged { .. }
            | SessionEvent::ModeChanged(_)
            | SessionEvent::SettingFailed { .. }
            | SessionEvent::Title(_)
            | SessionEvent::ContextUsage(_)
            | SessionEvent::TurnUsage(_)
            | SessionEvent::ContextReport(_)
            | SessionEvent::ContextReadable(_)
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
    if !switches().fade {
        return TextViewMotion::default();
    }
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
