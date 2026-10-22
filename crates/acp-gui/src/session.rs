//! One conversation with an agent as the window holds it: the rows of its transcript,
//! its settings with the changes the agent hasn't answered yet, and how each of the
//! agent's events for it lands there.

use std::collections::HashMap;
use std::path::PathBuf;

use acp_core::{PermissionRequest, SessionEvent, SessionKey, Setting, SettingKind, SettingValue};
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;

pub(crate) enum Row {
    User(String),
    /// Markdown, grown chunk by chunk as the agent streams.
    Agent(Entity<TextViewState>),
    Thought(Entity<TextViewState>),
    Tool {
        id: String,
        title: String,
        status: String,
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
    /// The first line of the first prompt, shown on the session's card.
    pub(crate) title: Option<String>,
    pub(crate) rows: Vec<Row>,
    pub(crate) scroller: Entity<MessageScrollerState>,
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
            rows: Vec::new(),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            busy: false,
            draft: SharedString::default(),
            settings: Vec::new(),
            created: false,
            asked: HashMap::new(),
            id: None,
            origin: Origin::New,
            archived: false,
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

    pub(crate) fn apply(&mut self, event: SessionEvent, cx: &mut App) {
        match event {
            SessionEvent::Created(id) => self.id = Some(id),
            SessionEvent::UserText(text) => self.append_user_text(&text, cx),
            SessionEvent::AgentText(text) => self.append_text(false, &text, cx),
            SessionEvent::AgentThought(text) => self.append_text(true, &text, cx),
            SessionEvent::ToolCall { id, title, status } => {
                self.push(Row::Tool { id, title, status }, cx)
            }
            SessionEvent::ToolCallUpdate { id, title, status } => {
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
                                if let Some(status) = status.clone() {
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
            SessionEvent::TurnEnded { stop_reason } => {
                self.busy = false;
                self.cancel_open_permissions(cx);
                if stop_reason != "EndTurn" {
                    self.push(Row::Notice(format!("turn ended: {stop_reason}")), cx);
                }
            }
            SessionEvent::TurnFailed(message) => {
                self.busy = false;
                self.cancel_open_permissions(cx);
                self.push(Row::Error(message), cx);
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
                self.push(Row::Error(message), cx)
            }
        }
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

    pub(crate) fn push(&mut self, row: Row, cx: &mut App) {
        self.rows.push(row);
        self.scroller.update(cx, |scroller, cx| {
            scroller.append(1, cx);
        });
    }

    fn remeasure(&mut self, ix: usize, cx: &mut App) {
        self.scroller.update(cx, |scroller, cx| {
            scroller.remeasure_items(ix..ix + 1, cx);
        });
    }
}

/// A selectable row's element id, which carries the session so a row of one session
/// never inherits the selection state of another session's row at the same index.
pub(crate) fn row_id(kind: &str, key: SessionKey, ix: usize) -> ElementId {
    ElementId::Name(format!("{kind}-{}-{ix}", key.0).into())
}
