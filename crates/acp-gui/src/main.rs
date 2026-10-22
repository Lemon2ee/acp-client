//! Minimal gpui front end: a sidebar of sessions beside a chat, driving ACP agents.
//! Usage: `acp-gui [claude | codex | "<agent command>"]`. With no argument it runs
//! both Claude and Codex, and the model menu picks between them.

use std::borrow::Cow;
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use acp_core::{
    AcpAgent, Command, Event as AgentEvent, Harness, PermissionRequest, Sender, SessionEvent,
    SessionKey, Setting, SettingChoice, SettingKind, SettingValue,
};
use gpui_kit::base::input::Copy;
use gpui_kit::base::{
    ElementExt as _, HandleEdge, SelectableText, TextSelection, Transition, transition,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    InputEvent, InputGroup, InputGroupAddon, InputGroupAddonAlignment, InputGroupButton, Textarea,
    TextareaState,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{
    ActiveTheme as _, FocusableExt as _, Icon, IconName, Sizable as _, Theme, ThemeConfig,
    ThemeStyled as _, h_flex, resize_handle, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

enum Row {
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
struct Session {
    key: SessionKey,
    /// Index into `ChatView::agents` of the agent it runs on. A session never moves:
    /// ACP has no way to carry one over to another agent.
    agent: usize,
    /// The first line of the first prompt, shown on the session's card.
    title: Option<String>,
    rows: Vec<Row>,
    scroller: Entity<MessageScrollerState>,
    /// Not ready for a prompt: a spare still being created, a turn is running, or the
    /// agent is gone. A session the agent hasn't created yet takes a prompt, which has
    /// the agent create it.
    busy: bool,
    /// Text typed here but not sent, kept while another session is on screen.
    draft: SharedString,
    /// What the agent lets the user choose for this session, such as its model; none
    /// until the agent has created it.
    settings: Vec<Setting>,
    /// The agent has created it and given its settings, if any.
    created: bool,
    /// Settings changed here whose changes the agent hasn't all answered, by setting id:
    /// switches, and every setting of a session the agent hasn't created yet, as no
    /// answer comes until it has.
    asked: HashMap<String, Asked>,
}

/// A setting changed here, until the agent has answered every change it was asked for.
struct Asked {
    /// The value last asked for, which the setting shows meanwhile.
    value: SettingValue,
    /// How many of its changes have no answer yet.
    out: u32,
}

impl Session {
    fn new(key: SessionKey, agent: usize, cx: &mut App) -> Self {
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
        }
    }

    /// A setting as it shows: one changed here shows the value last asked for until the
    /// agent has answered all its changes.
    fn shown(&self, setting: &Setting) -> Setting {
        let mut setting = setting.clone();
        if let Some(asked) = self.asked.get(&setting.id) {
            setting.value = asked.value.clone();
        }
        setting
    }

    /// The setting is to show `value` until the agent has answered the change just asked
    /// for, and any others still out.
    fn ask(&mut self, setting: String, value: SettingValue) {
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

    fn apply(&mut self, event: SessionEvent, cx: &mut App) {
        match event {
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
    fn cancel_open_permissions(&mut self, cx: &mut App) {
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

    fn answer(&mut self, ix: usize, choice: Option<String>, cx: &mut App) {
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

    fn push(&mut self, row: Row, cx: &mut App) {
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

/// One agent the window drives, and what it keeps ready on it.
struct AgentSlot {
    /// What the agent is called in the window: its harness's name, or the command it
    /// was started with.
    name: String,
    commands: Sender<Command>,
    /// A session created ahead of its use, so the model menu can list the models of an
    /// agent that has no session on show and no settings kept from before. Taken by
    /// the next session on it.
    spare: Option<Session>,
    /// The spare couldn't be created, and the user has been told; said once, as each
    /// session added tries the spare again.
    spare_failed: bool,
    /// The settings the agent last created a session with, in this run or, for Claude
    /// and Codex, one before, which a session on it shows until the agent has created
    /// it: no session is created before its first prompt, and creating one takes the
    /// agent seconds, as Claude's adapter starts the Claude Code CLI for it.
    last_settings: Vec<Setting>,
    _events: Task<()>,
}

struct ChatView {
    /// Never empty: the window starts with one session, which its first prompt has the
    /// agent create. At most one has no prompt yet; the sidebar leaves it out, and a new
    /// one on another agent takes its place.
    sessions: Vec<Session>,
    /// Index into `sessions` of the one on screen.
    active: usize,
    next_key: u64,
    input: Entity<TextareaState>,
    /// Never empty. New sessions go on the agent of the session on screen, until the
    /// user picks another agent's model.
    agents: Vec<AgentSlot>,
    /// The window's top row, which holds the traffic lights on macOS and the sidebar
    /// button: across the sidebar, or across the chat while the sidebar is hidden.
    title_bar_height: Pixels,
    /// Where the traffic lights end, from the window's left edge.
    traffic_lights_end: Pixels,
    sidebar_open: bool,
    /// How wide the sidebar is while shown. Dragged at its right edge.
    sidebar_width: Pixels,
    model_menu: ModelMenuState,
    /// Older Claude models the user added to the model list, by id. Kept on disk.
    added_models: Vec<String>,
    _input_events: Subscription,
}

impl ChatView {
    /// Starts every agent in `agents`, each by name, and shows a session on the first
    /// that no agent creates until its first prompt.
    fn new(
        agents: Vec<(String, AcpAgent)>,
        cwd: PathBuf,
        title_bar_height: Pixels,
        traffic_lights_end: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message the agent (Enter to send, Shift+Enter for a new line)")
                .auto_grow(1, 8)
                .submit_on_enter(true)
        });
        let input_events = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.send(window, cx);
            }
        });
        window.on_window_should_close(cx, |_, cx| {
            cx.quit();
            true
        });

        let mut last_settings = load_last_settings();
        let agents = agents
            .into_iter()
            .enumerate()
            .map(|(ix, (name, agent))| {
                let handle = acp_core::spawn(agent, cwd.clone());
                let events = handle.events;
                let events_task = cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        if this
                            .update(cx, |this, cx| this.apply(ix, event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                AgentSlot {
                    last_settings: kept_name(&name)
                        .and_then(|kept| last_settings.remove(kept))
                        .unwrap_or_default(),
                    name,
                    commands: handle.commands,
                    spare: None,
                    spare_failed: false,
                    _events: events_task,
                }
            })
            .collect();

        let mut view = Self {
            sessions: Vec::new(),
            active: 0,
            next_key: 0,
            input,
            agents,
            title_bar_height,
            traffic_lights_end,
            sidebar_open: true,
            sidebar_width: SIDEBAR_WIDTH,
            model_menu: ModelMenuState::default(),
            added_models: load_added_models(),
            _input_events: input_events,
        };
        view.new_session(window, cx);
        view
    }

    /// A session on the agent that the agent hasn't created yet: acp-core has it created
    /// by its first prompt. With the agent gone, the session says so and stays closed
    /// to prompts.
    fn create_session(&mut self, agent: usize, cx: &mut App) -> Session {
        let key = SessionKey(self.next_key);
        self.next_key += 1;
        let mut session = Session::new(key, agent, cx);
        let slot = &self.agents[agent];
        if slot.commands.is_closed() {
            session.busy = true;
            let message = format!("{} has gone; restart to use it", slot.name);
            session.push(Row::Error(message), cx);
        }
        session
    }

    /// Makes a spare session for each agent whose models the model menu could list no
    /// other way: it has no spare, no session on show, and no settings kept from before.
    /// None is made before the window's first prompt, so starting the window has no
    /// agent create a session.
    fn make_spares(&mut self, cx: &mut App) {
        if self.sessions.iter().all(|session| session.title.is_none()) {
            return;
        }
        for agent in 0..self.agents.len() {
            let slot = &self.agents[agent];
            if slot.spare.is_some()
                || !slot.last_settings.is_empty()
                || slot.commands.is_closed()
                || self.sessions.iter().any(|session| session.agent == agent)
            {
                continue;
            }
            let mut spare = self.create_session(agent, cx);
            // Created now rather than by a prompt, so its models are there to list.
            spare.busy = true;
            let slot = &mut self.agents[agent];
            let _ = slot.commands.try_send(Command::NewSession(spare.key));
            slot.spare = Some(spare);
        }
    }

    /// A session on the agent, to show: its spare, created with its settings, or one the
    /// agent will create on its first prompt.
    fn take_session(&mut self, agent: usize, cx: &mut App) -> Session {
        match self.agents[agent].spare.take() {
            Some(spare) => spare,
            None => self.create_session(agent, cx),
        }
    }

    /// The session whose settings say what the agent's models are: its spare, or the
    /// newest session on show on it.
    fn model_source(&self, agent: usize) -> Option<&Session> {
        let shown = self
            .sessions
            .iter()
            .rev()
            .filter(|session| session.agent == agent);
        self.agents[agent]
            .spare
            .as_ref()
            .into_iter()
            .chain(shown)
            .find(|session| !session.settings.is_empty())
    }

    /// The index of the window's session with no prompt yet on `agent`, made if there is
    /// none. One with no prompt on another agent gives way to it, so there is never more
    /// than one.
    fn untitled_on(&mut self, agent: usize, cx: &mut App) -> usize {
        let found = self
            .sessions
            .iter()
            .position(|session| session.title.is_none());
        if let Some(ix) = found.filter(|&ix| self.sessions[ix].agent == agent) {
            return ix;
        }
        let mut session = self.take_session(agent, cx);
        match found {
            Some(ix) => {
                // ACP can't move a session to another agent, so it goes. Text typed in
                // it while another session was on screen goes with the one taking its
                // place; on screen, that text is still in the input.
                session.draft = std::mem::take(&mut self.sessions[ix].draft);
                let old = std::mem::replace(&mut self.sessions[ix], session);
                let _ = self.agents[old.agent]
                    .commands
                    .try_send(Command::CloseSession(old.key));
                ix
            }
            None => {
                self.sessions.push(session);
                self.sessions.len() - 1
            }
        }
    }

    /// Shows a session with no prompt yet on the agent of the one on screen. The
    /// window's first session is added even if the agent is already gone, so there is
    /// always one to show.
    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self
            .sessions
            .get(self.active)
            .map_or(0, |session| session.agent);
        if self.agents[agent].commands.is_closed() && !self.sessions.is_empty() {
            return;
        }
        let ix = self.untitled_on(agent, cx);
        // The agent of a session with no prompt that gave way may have none on show
        // now, and an agent whose spare failed gets another go.
        self.make_spares(cx);
        self.show(ix, window, cx);
    }

    /// Picks a model of another agent's from the model menu of the session `from`, for
    /// the session with no prompt yet on that agent: as ACP can't move a session to
    /// another agent, one with no prompt on `from`'s agent gives way to it, and one with
    /// prompts keeps its agent. Nothing happens for an agent that is gone.
    fn pick_model(
        &mut self,
        from: SessionKey,
        agent: usize,
        setting: String,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.iter().all(|session| session.key != from)
            || self.agents[agent].commands.is_closed()
        {
            return;
        }
        let ix = self.untitled_on(agent, cx);
        let key = self.sessions[ix].key;
        self.change_setting(key, setting, SettingValue::Choice(model), cx);
        self.make_spares(cx);
        self.show(ix, window, cx);
    }

    /// Shows the session in the chat.
    fn select(&mut self, key: SessionKey, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.sessions.iter().position(|session| session.key == key) {
            self.show(ix, window, cx);
        }
    }

    /// Puts the session at `ix` on screen; unsent text stays with the session it was typed in.
    fn show(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix != self.active {
            self.sessions[self.active].draft = self.input.read(cx).value();
            self.active = ix;
            let draft = std::mem::take(&mut self.sessions[ix].draft);
            self.input.update(cx, |input, cx| {
                input.set_value(draft, window, cx);
                // `set_value` leaves a multi-line input's caret at the start. Collapsing a
                // select-all puts it at the end, and the next layout scrolls it into view;
                // `set_selected_range` would scroll using the replaced text's layout.
                input.select_all(window, cx);
                input.unselect(window, cx);
            });
        }
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// The session, shown or spare.
    fn session(&self, key: SessionKey) -> Option<&Session> {
        let spares = self.agents.iter().filter_map(|agent| agent.spare.as_ref());
        self.sessions
            .iter()
            .chain(spares)
            .find(|session| session.key == key)
    }

    /// The session, shown or spare.
    fn session_mut(&mut self, key: SessionKey) -> Option<&mut Session> {
        let spares = self
            .agents
            .iter_mut()
            .filter_map(|agent| agent.spare.as_mut());
        self.sessions
            .iter_mut()
            .chain(spares)
            .find(|session| session.key == key)
    }

    /// The session's settings as they show: the agent's for it once the agent has
    /// created it, and until then those it last created a session with. Either way a
    /// setting changed here shows the value asked for until the agent answers.
    fn shown_settings(&self, session: &Session) -> Vec<Setting> {
        let slot = &self.agents[session.agent];
        let settings = if session.created || slot.commands.is_closed() {
            &session.settings
        } else {
            &slot.last_settings
        };
        settings
            .iter()
            .map(|setting| session.shown(setting))
            .collect()
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        let session = &mut self.sessions[self.active];
        if session.busy || text.is_empty() {
            return;
        }
        self.input.update(cx, |input, cx| input.clean(window, cx));
        session
            .title
            .get_or_insert_with(|| text.lines().next().unwrap_or_default().to_string());
        session.busy = true;
        session.push(Row::User(text.clone()), cx);
        // Has the agent create the session first, if it hasn't yet.
        let prompt = Command::Prompt(session.key, text);
        let _ = self.agents[session.agent].commands.try_send(prompt);
        // The window's first prompt lets the agents that need one make a spare.
        self.make_spares(cx);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let session = &mut self.sessions[self.active];
        let _ = self.agents[session.agent]
            .commands
            .try_send(Command::Cancel(session.key));
        // Stopping makes acp-core answer the turn's open permission requests as cancelled.
        session.cancel_open_permissions(cx);
        cx.notify();
    }

    /// Picks one of a setting's choices. The setting shows its old value until the agent
    /// confirms the change, except in a session the agent hasn't created yet: acp-core
    /// holds the change until its first prompt has it created, so the pick shows until then.
    fn change_setting(
        &mut self,
        key: SessionKey,
        setting: String,
        value: SettingValue,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(key) else {
            return;
        };
        let (agent, created) = (session.agent, session.created);
        let change = Command::ChangeSetting(key, setting.clone(), value.clone());
        // With the agent gone, no answer would ever put the setting right.
        if self.agents[agent].commands.try_send(change).is_err() || created {
            return;
        }
        if let Some(session) = self.session_mut(key) {
            session.ask(setting, value);
        }
        cx.notify();
    }

    /// Flips one of the session's switches from the value it shows, which is the value
    /// last asked for until the agent answers, so a second flip before then turns it
    /// back. Unlike a pick from a list, the switch shows its new value at once.
    fn flip_switch(&mut self, key: SessionKey, setting: String, cx: &mut Context<Self>) {
        let Some(session) = self.session(key) else {
            return;
        };
        let agent = session.agent;
        let shown = self
            .shown_settings(session)
            .into_iter()
            .find(|shown| shown.id == setting)
            .map(|shown| shown.value);
        let Some(SettingValue::Switch(on)) = shown else {
            return;
        };
        let value = SettingValue::Switch(!on);
        let change = Command::ChangeSetting(key, setting.clone(), value.clone());
        // With the agent gone, no answer would ever put the switch right.
        if self.agents[agent].commands.try_send(change).is_err() {
            return;
        }
        if let Some(session) = self.session_mut(key) {
            session.ask(setting, value);
        }
        cx.notify();
    }

    /// The pointer came, at `position`, to a row of the model menu that wants `list` open
    /// beside the menu, and the older models open beside that list or not. A change that
    /// closes a list waits while the pointer heads for that list, as the way there can
    /// cross other rows; any other change is made at once.
    fn point_model_menu(
        &mut self,
        list: Option<String>,
        older: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = &mut self.model_menu;
        // Where the list the change would close was drawn.
        let closing = if menu.list.is_some() && menu.list != list {
            Some(menu.list_bounds.clone())
        } else if menu.older && !older {
            Some(menu.older_bounds.clone())
        } else {
            None
        };
        let heading = closing.filter(|bounds| {
            let from = menu.trail.front().copied();
            from.zip(bounds.get())
                .is_some_and(|(from, list)| heading_for(from, position, list))
        });
        let Some(bounds) = heading else {
            menu.pending = None;
            if menu.list != list || menu.older != older {
                menu.list = list;
                menu.older = older;
                cx.notify();
            }
            return;
        };
        menu.pending = Some(cx.spawn_in(window, async move |this, cx| {
            let mut from = position;
            loop {
                cx.background_executor().timer(MENU_HOVER_DELAY).await;
                // Still on its way if it moved on toward the list; otherwise it rests on
                // the row, and gets what the row wants.
                let moved_on = this.update_in(cx, |this, window, cx| {
                    let now = window.mouse_position();
                    let heading = now != from
                        && bounds
                            .get()
                            .is_some_and(|list| heading_for(from, now, list));
                    if !heading {
                        this.model_menu.list = list.clone();
                        this.model_menu.older = older;
                        cx.notify();
                    }
                    heading.then_some(now)
                });
                match moved_on {
                    Ok(Some(now)) => from = now,
                    _ => return,
                }
            }
        }));
    }

    /// Adds an older model to the model list, or takes it off again. Another window may
    /// have changed the list since this one read it, so the change is made to the list
    /// on disk, and this window takes that list up.
    fn set_model_added(&mut self, id: String, added: bool, cx: &mut Context<Self>) {
        let change = |models: &mut Vec<String>| {
            models.retain(|model| *model != id);
            if added {
                models.push(id.clone());
            }
        };
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| change_added_models(&folder, change));
        match changed {
            Ok(models) => self.added_models = models,
            Err(err) => {
                eprintln!("couldn't save the added models: {err}");
                // The list still changes for this run.
                change(&mut self.added_models);
            }
        }
        cx.notify();
    }

    /// Keeps the settings the agent created the session with as its last, here and on
    /// disk, unless they are the ones kept already. Only a session's first settings are
    /// those: later ones carry the changes made to it.
    fn remember_settings(&mut self, agent: usize, key: SessionKey, settings: &[Setting]) {
        let first = self
            .session_mut(key)
            .is_some_and(|session| !session.created);
        let slot = &mut self.agents[agent];
        if !first || slot.last_settings == settings {
            return;
        }
        slot.last_settings = settings.to_vec();
        let Some(kept) = kept_name(&slot.name) else {
            return;
        };
        let saved = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| save_last_settings(&folder, kept, settings));
        if let Err(err) = saved {
            eprintln!("couldn't save {}'s settings: {err}", slot.name);
        }
    }

    /// Takes an event from the agent at `agent`.
    fn apply(&mut self, agent: usize, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Connected { .. } => {}
            AgentEvent::SessionReady(key) => {
                if let Some(session) = self.session_mut(key) {
                    session.busy = false;
                }
                self.agents[agent].spare_failed = false;
            }
            AgentEvent::SessionFailed(key, message) => {
                let several = self.agents.len() > 1;
                let slot = &mut self.agents[agent];
                let message = if several {
                    format!("{}: {message}", slot.name)
                } else {
                    message
                };
                if slot.spare.as_ref().is_some_and(|spare| spare.key == key) {
                    // Nobody would prompt the spare to have it created again, so it goes,
                    // and the next session added makes another. Its error shows where
                    // the user is, once: every session added would repeat it otherwise.
                    slot.spare = None;
                    let _ = slot.commands.try_send(Command::CloseSession(key));
                    if !slot.spare_failed {
                        slot.spare_failed = true;
                        self.sessions[self.active].push(Row::Error(message), cx);
                    }
                } else if let Some(session) = self.session_mut(key) {
                    // Open for a prompt, which asks the agent to create the session again.
                    session.busy = false;
                    session.push(Row::Error(message), cx);
                }
            }
            AgentEvent::Session(key, event) => {
                if let SessionEvent::Settings(settings) = &event {
                    self.remember_settings(agent, key, settings);
                }
                if let Some(session) = self.session_mut(key) {
                    session.apply(event, cx);
                }
            }
            // Connection-wide failures show in the session on screen when it is the
            // agent's, else in the agent's newest session, else in the session on
            // screen. With several agents, the message says which one failed, as a
            // session's card doesn't.
            AgentEvent::Error(message) => {
                let active = self.active;
                let ix = Some(active)
                    .filter(|&ix| self.sessions[ix].agent == agent)
                    .or_else(|| {
                        self.sessions
                            .iter()
                            .rposition(|session| session.agent == agent)
                    })
                    .unwrap_or(active);
                let message = if self.agents.len() > 1 {
                    format!("{}: {message}", self.agents[agent].name)
                } else {
                    message
                };
                self.sessions[ix].push(Row::Error(message), cx)
            }
            AgentEvent::Closed => {
                let spare = self.agents[agent].spare.as_mut();
                for session in self.sessions.iter_mut().chain(spare) {
                    if session.agent != agent {
                        continue;
                    }
                    session.busy = true;
                    // A turn cut off with the connection may never report its end, nor
                    // a setting change its answer.
                    session.cancel_open_permissions(cx);
                    session.asked.clear();
                }
            }
        }
        cx.notify();
    }

    fn answer(
        &mut self,
        key: SessionKey,
        ix: usize,
        choice: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.session_mut(key) {
            session.answer(ix, choice, cx);
        }
        cx.notify();
    }

    fn render_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, bubble, danger, selection) = (
            theme.muted_foreground,
            theme.border,
            theme.secondary,
            theme.danger,
            theme.selection,
        );
        let session = &self.sessions[self.active];
        let key = session.key;
        let row = div().w_full().px_4().py_1p5();
        match &session.rows[ix] {
            Row::User(text) => row
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(0.8))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(bubble)
                        .child(
                            SelectableText::new(row_id("user", key, ix), text.clone())
                                .selection_color(selection),
                        ),
                )
                .into_any_element(),
            Row::Agent(view) => row
                .child(TextView::new(view).selectable(true))
                .into_any_element(),
            Row::Thought(view) => row
                .text_sm()
                .italic()
                .text_color(muted)
                .child(TextView::new(view).selectable(true))
                .into_any_element(),
            Row::Tool { title, status, .. } => row
                .text_sm()
                .text_color(muted)
                .child(
                    SelectableText::new(
                        row_id("tool", key, ix),
                        format!("tool: {title} ({status})"),
                    )
                    .selection_color(selection),
                )
                .into_any_element(),
            Row::Permission { request, decided } => {
                let card = div()
                    .mx_4()
                    .my_2()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .child(
                        SelectableText::new(
                            row_id("permission", key, ix),
                            format!("Permission requested: {}", request.title),
                        )
                        .selection_color(selection),
                    );
                let card = match decided {
                    Some(note) => card.child(div().text_sm().text_color(muted).child(note.clone())),
                    None => card.child(
                        div()
                            .flex()
                            .gap_2()
                            .children(request.options.iter().enumerate().map(
                                |(option_ix, option)| {
                                    let id = option.id.clone();
                                    let button = Button::new(ElementId::Name(
                                        format!("permission-{ix}-{option_ix}").into(),
                                    ))
                                    .label(option.name.clone())
                                    // A pointing hand, as over the send button.
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.answer(key, ix, Some(id.clone()), cx)
                                    }));
                                    if option.allow {
                                        button.primary()
                                    } else {
                                        button
                                    }
                                },
                            )),
                    ),
                };
                card.into_any_element()
            }
            Row::Notice(text) => row
                .text_sm()
                .text_color(muted)
                .child(
                    SelectableText::new(row_id("notice", key, ix), text.clone())
                        .selection_color(selection),
                )
                .into_any_element(),
            Row::Error(text) => row
                .text_sm()
                .text_color(danger)
                .child(
                    SelectableText::new(row_id("error", key, ix), text.clone())
                        .selection_color(selection),
                )
                .into_any_element(),
        }
    }
}

/// A selectable row's element id, which carries the session so a row of one session
/// never inherits the selection state of another session's row at the same index.
fn row_id(kind: &str, key: SessionKey, ix: usize) -> ElementId {
    ElementId::Name(format!("{kind}-{}-{ix}", key.0).into())
}

/// How wide the sidebar starts, and how narrow or wide a drag can make it. The minimum
/// keeps the sidebar button and the "New session" row whole.
const SIDEBAR_WIDTH: Pixels = px(240.);
const SIDEBAR_MIN_WIDTH: Pixels = px(180.);
const SIDEBAR_MAX_WIDTH: Pixels = px(480.);

/// What a drag of the sidebar's edge carries. gpui tells drags apart by this type, and
/// draws it under the pointer: nothing, since the sidebar itself follows the pointer.
#[derive(Clone)]
struct ResizeSidebar;

impl Render for ResizeSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The sidebar button's icon: a window outline with a short bar near its right side.
const SIDEBAR_ICON: &[u8] = include_bytes!("../assets/sidebar.svg");

/// On macOS the window has no title bar row: the sidebar and the chat both start at the
/// top edge, with the traffic lights in the window's top row. Elsewhere the system
/// title bar stays.
const OWN_TITLE_BAR: bool = cfg!(target_os = "macos");

/// Where the traffic lights go, and the top row that holds them, with as much space to
/// the lights' left and below them as above.
#[derive(Clone, Copy)]
struct TrafficLights {
    /// The close button's top-left corner.
    origin: Point<Pixels>,
    /// Each light's width and height.
    size: Pixels,
    row_height: Pixels,
}

impl TrafficLights {
    fn new(size: f32) -> Self {
        // The close button's center sits 21.5pt from the window's top and left edges.
        let inset = 21.5 - size / 2.;
        Self {
            origin: point(px(inset), px(inset)),
            size: px(size),
            row_height: px(2. * inset + size),
        }
    }

    /// Where the zoom button ends, from the window's left edge. gpui puts the close
    /// button at `origin` and keeps AppKit's spacing for the other two, which leaves
    /// 9pt between lights on macOS 27 (the fallback).
    fn end(&self, window: &Window) -> Pixels {
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::{NSView, NSWindowButton};
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};

            // `Window::window_handle` is gpui's own handle, not the platform's.
            if let Ok(handle) = HasWindowHandle::window_handle(window)
                && let RawWindowHandle::AppKit(handle) = handle.as_raw()
                // SAFETY: the handle's view is the window's content view, which lives as
                // long as the window.
                && let Some(ns_window) = unsafe { handle.ns_view.cast::<NSView>().as_ref() }.window()
                && let Some(close) = ns_window.standardWindowButton(NSWindowButton::CloseButton)
                && let Some(zoom) = ns_window.standardWindowButton(NSWindowButton::ZoomButton)
            {
                let (close, zoom) = (close.frame(), zoom.frame());
                return self.origin.x
                    + px((zoom.origin.x + zoom.size.width - close.origin.x) as f32);
            }
        }
        let _ = window;
        self.origin.x + self.size * 3. + px(2. * 9.)
    }
}

/// The traffic lights' size as AppKit lays them out: 14pt on macOS 27, which is also
/// the fallback.
fn traffic_light_size() -> f32 {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSWindow, NSWindowButton, NSWindowStyleMask};
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        if let Some(mtm) = objc2::MainThreadMarker::new()
            && let Some(button) =
                NSWindow::standardWindowButton_forStyleMask(NSWindowButton::CloseButton, style, mtm)
        {
            return button.frame().size.height as f32;
        }
    }
    14.
}

/// Makes `strip` stand in for the title bar the window doesn't have: dragging it moves
/// the window, and a double-click does what the system setting for title bars says.
/// The strip must not hold anything clickable.
fn as_title_bar(strip: Div) -> Div {
    strip.on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count == 2 {
            window.titlebar_double_click();
        } else {
            // AppKit moves the window with the mouse from this press until the button
            // comes up, and keeps that mouse-up to itself. A press that doesn't move
            // leaves the window where it is.
            window.start_window_move();
        }
    })
}

/// How tall the model menu's rows are, and the gap between them.
const MENU_ROW_HEIGHT: Pixels = px(36.);
const MENU_ROW_GAP: Pixels = px(2.);
/// How tall the rows of a list beside the model menu are.
const LIST_ROW_HEIGHT: Pixels = px(32.);
/// How tall the line naming an agent above its models is, in the model list.
const LIST_HEADER_HEIGHT: Pixels = px(24.);
/// How long a change to the model menu's open lists waits, while the pointer heads for
/// a list the change would close, before looking again at where the pointer went.
const MENU_HOVER_DELAY: Duration = Duration::from_millis(300);
/// Groups each row of a list beside the model menu with its button, which shows while
/// the row is pointed at.
const CHOICE_GROUP: &str = "model-menu-choice";

/// Claude's model families. Its models are named "<family> <version>", like "Opus 4.7".
const CLAUDE_FAMILIES: [&str; 4] = ["Fable", "Opus", "Sonnet", "Haiku"];

/// A Claude model's family and version, read from its name.
fn claude_model(name: &str) -> Option<(&str, Vec<u32>)> {
    let mut words = name.split(' ');
    let family = words
        .next()
        .filter(|family| CLAUDE_FAMILIES.contains(family))?;
    let version = words
        .next()?
        .split('.')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    Some((family, version))
}

/// Claude's models as the model list shows them.
struct ClaudeModels<'a> {
    /// The newest model of each family and the older ones the user added, with whether
    /// the user added it.
    listed: Vec<(&'a SettingChoice, bool)>,
    /// The other older models, a level down.
    older: Vec<&'a SettingChoice>,
}

/// Sorts Claude's models out for the model list, each part by family, newest first.
/// The adapter's "default" entry is left out: it stands for one of the others. `None`
/// when the choices aren't Claude's models.
fn claude_models<'a>(choices: &'a [SettingChoice], added: &[String]) -> Option<ClaudeModels<'a>> {
    let models: Vec<_> = choices
        .iter()
        .map(|choice| (choice, claude_model(&choice.name)))
        .collect();
    // Each family in the order the adapter first lists it, with its newest version.
    let mut families: Vec<(&str, &[u32])> = Vec::new();
    for (family, version) in models.iter().filter_map(|(_, model)| model.as_ref()) {
        match families.iter_mut().find(|(name, _)| name == family) {
            Some((_, newest)) => *newest = (*newest).max(version.as_slice()),
            None => families.push((family, version)),
        }
    }
    if families.is_empty() {
        return None;
    }
    // Models of no family come last, as the adapter lists them.
    let order = |model: &Option<(&str, Vec<u32>)>| match model {
        Some((family, version)) => (
            families.iter().position(|(name, _)| name == family),
            Reverse(version.clone()),
        ),
        None => (Some(usize::MAX), Reverse(Vec::new())),
    };
    let mut listed = Vec::new();
    let mut older = Vec::new();
    for (choice, model) in &models {
        if choice.id == "default" {
            continue;
        }
        let is_older = model.as_ref().is_some_and(|(family, version)| {
            families
                .iter()
                .any(|(name, newest)| name == family && version.as_slice() < *newest)
        });
        match (is_older, added.contains(&choice.id)) {
            (true, false) => older.push((*choice, model)),
            (is_older, _) => listed.push(((*choice, is_older), model)),
        }
    }
    listed.sort_by_key(|(_, model)| order(model));
    older.sort_by_key(|(_, model)| order(model));
    Some(ClaudeModels {
        listed: listed.into_iter().map(|(entry, _)| entry).collect(),
        older: older.into_iter().map(|(choice, _)| choice).collect(),
    })
}

/// The choice a setting shows as its value. Claude's adapter can set the model to its
/// "default" entry, which isn't listed; its description names the model it stands for.
fn shown_choice(setting: &Setting) -> Option<&SettingChoice> {
    let chosen = setting.chosen()?;
    if setting.kind != SettingKind::Model || chosen.id != "default" {
        return Some(chosen);
    }
    let stands_for = chosen.description.as_deref();
    let model = setting
        .choices
        .iter()
        .find(|choice| choice.id != "default" && Some(choice.name.as_str()) == stands_for);
    Some(model.unwrap_or(chosen))
}

/// What the model menu keeps from frame to frame while it is open.
#[derive(Default)]
struct ModelMenuState {
    /// The row whose choices are open beside the menu, by its setting's id.
    list: Option<String>,
    /// Whether the older models are open beside the model list.
    older: bool,
    /// A change to the open lists, waiting while the pointer heads for one of them.
    pending: Option<Task<()>>,
    /// The last few places the pointer moved to over the menu, oldest first.
    trail: VecDeque<Point<Pixels>>,
    /// Where the model list and the older models were last drawn.
    list_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    older_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// Whether the pointer, moving from `from` to `to`, is heading for `list`, which is to
/// its right: `to` lies in the triangle between `from` and the list's left edge, made a
/// little taller than the list.
fn heading_for(from: Point<Pixels>, to: Point<Pixels>, list: Bounds<Pixels>) -> bool {
    const SLACK: f32 = 16.;
    let at = |point: Point<Pixels>| (f32::from(point.x), f32::from(point.y));
    let (a, p) = (at(from), at(to));
    let left = f32::from(list.left());
    let b = (left, f32::from(list.top()) - SLACK);
    let c = (left, f32::from(list.bottom()) + SLACK);
    // Which side of the line from `u` to `v` the pointer is on.
    let side = |u: (f32, f32), v: (f32, f32)| (v.0 - u.0) * (p.1 - u.1) - (v.1 - u.1) * (p.0 - u.0);
    let sides = [side(a, b), side(b, c), side(c, a)];
    p.0 > a.0 && (sides.iter().all(|side| *side >= 0.) || sides.iter().all(|side| *side <= 0.))
}

/// Another agent's models, listed in the model menu of a session on this one.
struct OtherModels {
    agent: usize,
    name: String,
    setting: Setting,
}

/// What the parts of the model menu share.
struct ModelMenu<'a> {
    view: &'a WeakEntity<ChatView>,
    key: SessionKey,
    /// The name of the session's agent, which heads its models when there are others.
    own_name: &'a str,
    /// The other agents' models, each group under its agent's name.
    others: &'a [OtherModels],
    popover: &'a Entity<PopoverState>,
    list_bounds: &'a Rc<Cell<Option<Bounds<Pixels>>>>,
    older_bounds: &'a Rc<Cell<Option<Bounds<Pixels>>>>,
    accent: Hsla,
    accent_foreground: Hsla,
    muted: Hsla,
    /// How the component library's menus round their rows.
    radius: Pixels,
}

impl ModelMenu<'_> {
    /// While the row is pointed at, it wants `list` open beside the menu, and the older
    /// models open beside that list or not.
    fn wants(
        &self,
        list: Option<String>,
        older: bool,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let view = self.view.clone();
        move |hovered, window, cx| {
            if *hovered {
                let position = window.mouse_position();
                let _ = view.update(cx, |this, cx| {
                    this.point_model_menu(list.clone(), older, position, window, cx)
                });
            }
        }
    }

    /// Pointing at an open list keeps it open, whatever rows the pointer crossed.
    fn holds(&self) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let view = self.view.clone();
        move |hovered, _, cx| {
            if *hovered {
                let _ = view.update(cx, |this, _| this.model_menu.pending = None);
            }
        }
    }

    /// Notes where the pointer moves over the menu, to tell where it is heading. A row
    /// hears that the pointer came to it before this does, so the note holds where the
    /// pointer was before.
    fn trails(&self) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
        let view = self.view.clone();
        move |event, _, cx| {
            let _ = view.update(cx, |this, _| {
                let trail = &mut this.model_menu.trail;
                trail.push_back(event.position);
                // Three moves back is far enough to tell a direction by.
                if trail.len() > 3 {
                    trail.pop_front();
                }
            });
        }
    }

    /// A row of a list beside the menu, which sets `setting` to its choice and closes
    /// the menu. `button`, if any, shows while the row is pointed at.
    fn choice(
        &self,
        setting: &Setting,
        choice: &SettingChoice,
        current: bool,
        button: Option<AnyElement>,
    ) -> Stateful<Div> {
        let (view, popover, key) = (self.view.clone(), self.popover.clone(), self.key);
        let (id, value) = (setting.id.clone(), choice.id.clone());
        let (accent, accent_foreground) = (self.accent, self.accent_foreground);
        div()
            .id(ElementId::Name(
                format!("model-menu-choice-{}", choice.id).into(),
            ))
            .group(CHOICE_GROUP)
            .h(LIST_ROW_HEIGHT)
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(self.radius)
            .hover(move |style| style.bg(accent).text_color(accent_foreground))
            .child(
                div()
                    .flex_1()
                    .child(SharedString::from(choice.name.clone())),
            )
            .children(button)
            .when(current, |row| {
                row.child(Icon::new(IconName::Check).xsmall())
            })
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.change_setting(key, id.clone(), SettingValue::Choice(value.clone()), cx)
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
    }

    /// A row of the model list for another agent's model, which goes on to a session
    /// on that agent with the model, and closes the menu.
    fn other_choice(&self, other: &OtherModels, choice: &SettingChoice) -> Stateful<Div> {
        let (view, popover, key) = (self.view.clone(), self.popover.clone(), self.key);
        let (agent, id, value) = (other.agent, other.setting.id.clone(), choice.id.clone());
        let (accent, accent_foreground) = (self.accent, self.accent_foreground);
        div()
            .id(ElementId::Name(
                format!("model-menu-other-{agent}-{}", choice.id).into(),
            ))
            .h(LIST_ROW_HEIGHT)
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(self.radius)
            .hover(move |style| style.bg(accent).text_color(accent_foreground))
            .child(
                div()
                    .flex_1()
                    .child(SharedString::from(choice.name.clone())),
            )
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.pick_model(key, agent, id.clone(), value.clone(), window, cx)
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
    }

    /// A line in the model list naming the agent whose models follow it.
    fn group_header(&self, name: &str) -> AnyElement {
        div()
            .h(LIST_HEADER_HEIGHT)
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .text_xs()
            .text_color(self.muted)
            .child(SharedString::from(name.to_string()))
            .into_any_element()
    }

    /// The button on an older model's row that adds it to the model list, or takes it
    /// off again. It shows while its row is pointed at.
    fn add_button(&self, id: &str, add: bool) -> AnyElement {
        let view = self.view.clone();
        let id = id.to_string();
        div()
            .flex_none()
            // The button draws its icon 4px in from its edge. This puts the icon against
            // the row's padding, where a row's check goes.
            .mr(px(-4.))
            .opacity(0.)
            .group_hover(CHOICE_GROUP, |style| style.opacity(1.))
            .child(
                Button::new(ElementId::Name(format!("model-menu-add-{id}").into()))
                    .ghost()
                    .xsmall()
                    .icon(if add { IconName::Plus } else { IconName::Minus })
                    .tooltip(if add {
                        "Add to the list"
                    } else {
                        "Remove from the list"
                    })
                    .on_click(move |_, _, cx| {
                        // Doesn't pick the model, as a press elsewhere on its row does.
                        cx.stop_propagation();
                        let _ =
                            view.update(cx, |this, cx| this.set_model_added(id.clone(), add, cx));
                    }),
            )
            .into_any_element()
    }

    /// A list beside the menu or beside another list, its bottom `bottom` above the
    /// bottom of what it is beside. Where it is drawn goes in `bounds`.
    fn list(
        &self,
        id: &'static str,
        rows: Vec<AnyElement>,
        bottom: Pixels,
        bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
        window: &Window,
        cx: &App,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .absolute()
            .left(relative(1.))
            .ml_1()
            .bottom(bottom)
            .min_w(px(160.))
            .popover_style(cx)
            .p_1()
            .flex()
            .flex_col()
            .gap(MENU_ROW_GAP)
            // It lies outside the popover, which keeps the pointer from what is under
            // the menu but not from what is under its lists.
            .occlude()
            .on_hover(self.holds())
            .on_mouse_move(self.trails())
            .child(
                // Scrolls inside the frame, so the frame's bounds stay where it is drawn.
                div()
                    .id("rows")
                    .max_h(window.viewport_size().height * 0.7)
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(MENU_ROW_GAP)
                    .children(rows),
            )
            .on_prepaint(move |frame, _, _| bounds.set(Some(frame)))
    }
}

/// The model menu: a row for each setting, and beside the row whose list is open, a
/// list of that setting's choices. Claude's older models are a level further down.
/// Each list's bottom is level with the row it opens from, so it opens upward like
/// the menu. With other agents to pick from, the model list has a group for each
/// agent, the session's own first.
fn model_menu(
    menu: &ModelMenu,
    settings: &[Setting],
    open_list: Option<&str>,
    older_open: bool,
    added: &[String],
    window: &Window,
    cx: &App,
) -> Div {
    let (accent, accent_foreground, muted) = (menu.accent, menu.accent_foreground, menu.muted);
    let border = cx.theme().border;
    // Where the open lists are drawn, so a press in them leaves the menu open, and the
    // pointer's way to them can be told. Set as they are drawn; a closed list has none.
    let (list_bounds, older_bounds) = (menu.list_bounds.clone(), menu.older_bounds.clone());
    list_bounds.set(None);
    older_bounds.set(None);

    // A model row's list holds the other agents' models too, so it opens for them
    // even when the session's own agent offers no model.
    let has_others =
        |setting: &Setting| setting.kind == SettingKind::Model && !menu.others.is_empty();
    let has_list = |setting: &Setting| !setting.choices.is_empty() || has_others(setting);
    let rows = settings.iter().map(|setting| {
        let list = has_list(setting).then(|| setting.id.clone());
        let row = div()
            .id(ElementId::Name(format!("model-menu-{}", setting.id).into()))
            .h(MENU_ROW_HEIGHT)
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(menu.radius)
            .hover(move |style| style.bg(accent).text_color(accent_foreground))
            .child(
                div()
                    .flex_1()
                    .child(SharedString::from(setting.name.clone())),
            )
            .on_hover(menu.wants(list, false));
        match &setting.value {
            SettingValue::Switch(on) => {
                let (view, key, id) = (menu.view.clone(), menu.key, setting.id.clone());
                row.child(
                    // Only shows the value: a press anywhere on the row flips it.
                    Switch::new(ElementId::Name(
                        format!("model-menu-switch-{}", setting.id).into(),
                    ))
                    .checked(*on)
                    .tab_stop(false),
                )
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.flip_switch(key, id.clone(), cx));
                })
            }
            SettingValue::Choice(_) => row
                .when(open_list == Some(setting.id.as_str()), |row| {
                    row.bg(accent).text_color(accent_foreground)
                })
                .children(shown_choice(setting).map(|choice| {
                    div()
                        .text_color(muted)
                        .child(SharedString::from(choice.name.clone()))
                }))
                .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted)),
        }
    });

    let list = open_list
        .and_then(|id| settings.iter().position(|setting| setting.id == id))
        .filter(|&ix| has_list(&settings[ix]))
        .map(|ix| {
            let setting = &settings[ix];
            let current = shown_choice(setting).map(|choice| choice.id.as_str());
            let claude = (setting.kind == SettingKind::Model)
                .then(|| claude_models(&setting.choices, added))
                .flatten();
            let own = match &claude {
                Some(models) => models
                    .listed
                    .iter()
                    .map(|&(choice, added)| {
                        let button = added.then(|| menu.add_button(&choice.id, false));
                        menu.choice(setting, choice, current == Some(choice.id.as_str()), button)
                    })
                    .collect::<Vec<_>>(),
                None => setting
                    .choices
                    .iter()
                    .map(|choice| {
                        menu.choice(setting, choice, current == Some(choice.id.as_str()), None)
                    })
                    .collect(),
            };
            // Pointing at this list's rows closes the older models beside it.
            let hold = |row: Stateful<Div>| {
                row.on_hover(menu.wants(Some(setting.id.clone()), false))
                    .into_any_element()
            };
            let grouped = has_others(setting);
            let mut rows: Vec<AnyElement> = Vec::new();
            if grouped && !own.is_empty() {
                rows.push(menu.group_header(menu.own_name));
            }
            rows.extend(own.into_iter().map(hold));
            // The other agents' models, each group under its agent's name, go below
            // the session's own agent's, after its older models' row, so that row
            // reads as Claude's. They don't scroll with the own agent's rows, so the
            // older models' list can be set level with its row by their height.
            let mut other_groups: Vec<AnyElement> = Vec::new();
            let mut others_height = px(0.);
            for other in menu.others.iter().filter(|_| grouped) {
                other_groups.push(
                    div()
                        .flex_none()
                        .h(px(1.))
                        .mx_neg_1()
                        .bg(border)
                        .into_any_element(),
                );
                other_groups.push(menu.group_header(&other.name));
                let listed: Vec<&SettingChoice> = match claude_models(&other.setting.choices, added)
                {
                    Some(models) => models
                        .listed
                        .into_iter()
                        .map(|(choice, _)| choice)
                        .collect(),
                    None => other.setting.choices.iter().collect(),
                };
                others_height += px(1.)
                    + LIST_HEADER_HEIGHT
                    + MENU_ROW_GAP * 2.
                    + (LIST_ROW_HEIGHT + MENU_ROW_GAP) * listed.len() as f32;
                other_groups.extend(
                    listed
                        .into_iter()
                        .map(|choice| hold(menu.other_choice(other, choice))),
                );
            }
            let rows_below = (settings.len() - 1 - ix) as f32;
            let frame = menu.list(
                "model-menu-list",
                rows,
                (MENU_ROW_HEIGHT + MENU_ROW_GAP) * rows_below,
                list_bounds.clone(),
                window,
                cx,
            );
            let older = claude.filter(|models| !models.older.is_empty());
            frame
                .when_some(older, |frame, models| {
                    let older_list = older_open.then(|| {
                        let rows = models
                            .older
                            .iter()
                            .map(|&choice| {
                                let button = Some(menu.add_button(&choice.id, true));
                                menu.choice(
                                    setting,
                                    choice,
                                    current == Some(choice.id.as_str()),
                                    button,
                                )
                                .into_any_element()
                            })
                            .collect();
                        // Level with the row that opens it, above the other agents' groups.
                        menu.list(
                            "model-menu-older",
                            rows,
                            others_height,
                            older_bounds.clone(),
                            window,
                            cx,
                        )
                    });
                    frame
                        .child(
                            // A line across the list, above the row for the older models.
                            div().flex_none().h(px(1.)).mx_neg_1().bg(cx.theme().border),
                        )
                        .child(
                            div()
                                .id("model-menu-older-row")
                                .h(LIST_ROW_HEIGHT)
                                .flex_none()
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded(menu.radius)
                                .hover(move |style| style.bg(accent).text_color(accent_foreground))
                                .when(older_open, |row| {
                                    row.bg(accent).text_color(accent_foreground)
                                })
                                .child(div().flex_1().child("Older models"))
                                .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted))
                                .on_hover(menu.wants(Some(setting.id.clone()), true)),
                        )
                        .children(older_list)
                })
                .children(other_groups)
        });

    let popover = menu.popover.clone();
    div()
        .relative()
        .min_w(px(250.))
        .popover_style(cx)
        .p_1()
        .flex()
        .flex_col()
        .gap(MENU_ROW_GAP)
        .text_sm()
        // Rows are one line tall. A list's room is reckoned from the width of what it is
        // placed against, so its names would wrap there too.
        .whitespace_nowrap()
        .on_mouse_move(menu.trails())
        .on_mouse_down_out(move |_, window, cx| {
            let position = window.mouse_position();
            let in_list = [&list_bounds, &older_bounds].iter().any(|bounds| {
                bounds
                    .get()
                    .is_some_and(|bounds| bounds.contains(&position))
            });
            if !in_list {
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            }
        })
        .children(rows)
        .children(list)
}

/// A row of the mode menu: the mode's icon and name, what it does below the name, and
/// a check after the current mode. The icon and the check sit level with the name.
fn mode_item(choice: &SettingChoice, current: bool) -> PopupMenuItem {
    let icon = mode_icon(&choice.id);
    let name = SharedString::from(choice.name.clone());
    let description = choice.description.clone().map(SharedString::from);
    PopupMenuItem::element(move |_, cx| {
        let muted = cx.theme().muted_foreground;
        // A box as tall as the name's line.
        let line = || div().flex_none().h(px(20.)).flex().items_center();
        h_flex()
            .flex_1()
            .items_start()
            .gap_2()
            .py_1p5()
            .child(line().child(Icon::new(icon).size(px(16.))))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .child(div().line_height(px(20.)).child(name.clone()))
                    .children(
                        description.clone().map(|description| {
                            div().text_xs().text_color(muted).child(description)
                        }),
                    ),
            )
            // Kept when empty, so every row's text wraps at the same width.
            .child(line().w(px(12.)).ml_1().when(current, |line| {
                line.child(Icon::new(IconName::Check).xsmall())
            }))
    })
}

/// The icon for a mode, by the ids Claude's and Codex's adapters give their modes.
fn mode_icon(id: &str) -> gpui_kit::assets::IconName {
    use gpui_kit::assets::IconName;
    match id {
        // Asks before every change.
        "default" => IconName::Hand,
        "acceptEdits" => IconName::FilePen,
        "plan" => IconName::ListChecks,
        "read-only" => IconName::Eye,
        "workspace-write" => IconName::FolderPen,
        // Asks only about what the agent judges unsafe.
        "auto" | "agent" => IconName::ShieldCheck,
        // Never asks.
        "bypassPermissions" | "agent-full-access" => IconName::ShieldOff,
        _ => IconName::Shield,
    }
}

/// The kinds of the model's settings, in the order its menu lists them.
const MODEL_KINDS: [SettingKind; 3] = [
    SettingKind::ModelConfig,
    SettingKind::ThoughtLevel,
    SettingKind::Model,
];

/// The model's button, named like "Opus 5.5 High" after the model and how hard it
/// thinks. An agent that offers neither still gets the button, for its other settings
/// of the model.
fn model_button(settings: &[Setting]) -> InputGroupButton {
    let names: Vec<&str> = [SettingKind::Model, SettingKind::ThoughtLevel]
        .into_iter()
        .filter_map(|kind| shown_choice(settings.iter().find(|setting| setting.kind == kind)?))
        .map(|choice| choice.name.as_str())
        .collect();
    let label = if names.is_empty() {
        "Model".to_string()
    } else {
        names.join(" ")
    };
    InputGroupButton::new("model-picker")
        .label(label.clone())
        .accessibility_label(format!("Model: {label}"))
        .dropdown_caret(true)
        .small()
}

/// The mode's button, with the mode's icon.
fn mode_button(setting: &Setting) -> InputGroupButton {
    let current = setting.chosen();
    let name = current.map_or_else(|| setting.name.clone(), |choice| choice.name.clone());
    let icon = mode_icon(current.map_or("", |choice| choice.id.as_str()));
    InputGroupButton::new("mode-picker")
        .icon(icon)
        .label(name.clone())
        .accessibility_label(format!("{}: {name}", setting.name))
        .dropdown_caret(true)
        .small()
}

impl ChatView {
    /// The button for the model and how hard it thinks, named like "Opus 5.5 High",
    /// once the session's settings, `shown`, hold any of the model's. Its menu opens
    /// upward: a switch for each of them that is on or off, then a row for each of the
    /// others, whose choices open beside it.
    fn render_model_picker(
        &self,
        session: &Session,
        shown: &[Setting],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let settings: Vec<Setting> = MODEL_KINDS
            .into_iter()
            .flat_map(|kind| shown.iter().filter(move |setting| setting.kind == kind))
            .cloned()
            .collect();
        // The other agents' models: as a session on each has them, else as the agent
        // last created a session with them. An agent that is gone can't take a
        // session, so its models aren't offered.
        let others: Vec<OtherModels> = self
            .agents
            .iter()
            .enumerate()
            .filter(|&(ix, slot)| ix != session.agent && !slot.commands.is_closed())
            .filter_map(|(ix, slot)| {
                let setting = self
                    .model_source(ix)
                    .map_or(&slot.last_settings, |source| &source.settings)
                    .iter()
                    .find(|setting| setting.kind == SettingKind::Model)?;
                Some(OtherModels {
                    agent: ix,
                    name: slot.name.clone(),
                    setting: setting.clone(),
                })
            })
            .collect();
        let mut settings = settings;
        if !settings
            .iter()
            .any(|setting| setting.kind == SettingKind::Model)
        {
            if others.is_empty() {
                if settings.is_empty() {
                    return None;
                }
            } else {
                // A session with no model to pick, as its agent offers none or hasn't
                // said yet which, still gets to the other agents' models from a model row.
                settings.push(Setting {
                    id: "model".into(),
                    name: "Model".into(),
                    kind: SettingKind::Model,
                    value: SettingValue::Choice(String::new()),
                    choices: Vec::new(),
                });
            }
        }
        let own_name = self.agents[session.agent].name.clone();
        let view = cx.entity().downgrade();
        let key = session.key;
        let open_list = self.model_menu.list.clone();
        let older_open = self.model_menu.older;
        let list_bounds = self.model_menu.list_bounds.clone();
        let older_bounds = self.model_menu.older_bounds.clone();
        let added = self.added_models.clone();
        let trigger = model_button(shown).cursor_pointer();
        let picker = Popover::new("model-menu")
            .appearance(false)
            // The menu closes itself on a press outside it and its open lists.
            .overlay_closable(false)
            .anchor(Anchor::BottomLeft)
            .trigger(trigger)
            .on_open_change({
                let view = view.clone();
                move |_, _, cx| {
                    // It opens with no list beside it, each time.
                    let _ = view.update(cx, |this, cx| {
                        this.model_menu = ModelMenuState::default();
                        cx.notify();
                    });
                }
            })
            .content(move |_, window, cx| {
                let popover = cx.entity();
                let theme = cx.theme();
                let menu = ModelMenu {
                    view: &view,
                    key,
                    own_name: &own_name,
                    others: &others,
                    popover: &popover,
                    list_bounds: &list_bounds,
                    older_bounds: &older_bounds,
                    accent: *theme.tokens.accent,
                    accent_foreground: theme.accent_foreground,
                    muted: theme.muted_foreground,
                    radius: theme.radius.min(px(8.)),
                };
                model_menu(
                    &menu,
                    &settings,
                    open_list.as_deref(),
                    older_open,
                    &added,
                    window,
                    cx,
                )
            });
        Some(picker.into_any_element())
    }

    /// The mode's button, with the mode's icon, and a menu of the modes that opens
    /// upward, each with its icon and what the agent says it does.
    fn render_mode_picker(
        &self,
        key: SessionKey,
        setting: &Setting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let setting = setting.clone();
        mode_button(&setting)
            .cursor_pointer()
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                // Wide enough for most descriptions on one line; longer ones wrap.
                let menu = menu.min_w(px(260.)).max_w(px(360.));
                setting.choices.iter().fold(menu, |menu, choice| {
                    let current = setting.value == SettingValue::Choice(choice.id.clone());
                    let (view, id, value) = (view.clone(), setting.id.clone(), choice.id.clone());
                    menu.item(mode_item(choice, current).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.change_setting(
                                key,
                                id.clone(),
                                SettingValue::Choice(value.clone()),
                                cx,
                            )
                        });
                    }))
                })
            })
            .into_any_element()
    }

    fn render_sidebar(&self, title_bar: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (foreground, hover) = (theme.sidebar_foreground, theme.secondary);
        let (accent, accent_foreground) = (theme.sidebar_accent, theme.sidebar_accent_foreground);
        // Newest first; the session on screen gets a filled card. The one with no prompt
        // yet has none: the agent hasn't created it, and it may give way to another.
        let cards = self
            .sessions
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(ix, session)| Some((ix, session.key, session.title.clone()?)))
            .map(|(ix, key, title)| {
                let title = SharedString::from(title);
                div()
                    .id(ElementId::Name(format!("session-{}", key.0).into()))
                    .role(Role::Button)
                    .aria_label(title.clone())
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .map(|card| {
                        if ix == self.active {
                            card.bg(accent)
                                .text_color(accent_foreground)
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            card.hover(|style| style.bg(accent.opacity(0.5)))
                        }
                    })
                    .child(div().flex_1().min_w(px(0.)).truncate().child(title))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select(key, window, cx)
                    }))
            });
        div()
            .w(self.sidebar_width)
            .flex_none()
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .text_color(foreground)
            .child(
                // The line between the sidebar and the chat, which goes with the sidebar,
                // and the band over it that drags the sidebar's width: the root follows
                // the drag. A double-click puts the width back.
                resize_handle("sidebar-edge", Axis::Horizontal)
                    .inside(HandleEdge::Trailing)
                    .on_drag(ResizeSidebar, |_, _, _, cx| cx.new(|_| ResizeSidebar))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        if event.click_count() == 2 {
                            this.sidebar_width = SIDEBAR_WIDTH;
                            cx.notify();
                        }
                    })),
            )
            .child(
                // Nothing goes here: the traffic lights and the sidebar button are drawn
                // over this row.
                div()
                    .flex_none()
                    .h(self.title_bar_height)
                    .when(title_bar, as_title_bar),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .child(
                        // A plain div: the component Button always centers its label.
                        div()
                            .id("new-session")
                            .role(Role::Button)
                            .aria_label("New session")
                            .h(px(32.))
                            // The plus starts 1.5px further into its icon than a title's
                            // first letter does into its text, so the left padding is
                            // 1.5px short of the cards' to line the plus up with them.
                            .pl(px(10.5))
                            .pr_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .hover(|style| style.bg(hover))
                            .child(Icon::new(IconName::Plus).small())
                            .child("New session")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.new_session(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("sessions")
                            .flex_1()
                            .min_h(px(0.))
                            .mt_2()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .overflow_y_scroll()
                            .children(cards),
                    ),
            )
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // In full screen the traffic lights go back to the system's title bar, which
        // slides down over the window, so the window keeps none of its own.
        let title_bar = OWN_TITLE_BAR && !window.is_fullscreen();
        let theme = cx.theme();
        let (background, foreground, sidebar, input_border) = (
            theme.background,
            theme.foreground,
            theme.sidebar,
            theme.input,
        );
        let (muted, hover) = (theme.muted_foreground, theme.secondary);
        let motion = theme.motion_tokens();
        let slide = Transition::new(motion.duration_slow).easing(motion.easing_move.clone());
        // 0 while the sidebar is shown and 1 while it is hidden, in between as it slides.
        let hidden = transition(
            "sidebar-hidden",
            if self.sidebar_open { 0. } else { 1. },
            slide,
            window,
            cx,
        );
        let sidebar_width = self.sidebar_width * (1. - hidden);
        let session = &self.sessions[self.active];
        // The model and how hard it thinks, then the mode, each once the agent has said
        // what it can be, for this session or the last it created.
        let shown = self.shown_settings(session);
        let pickers = [
            self.render_model_picker(session, &shown, cx),
            shown
                .iter()
                .find(|setting| setting.kind == SettingKind::Mode)
                .map(|setting| self.render_mode_picker(session.key, setting, cx)),
        ];
        let action = if session.busy {
            InputGroupButton::new("stop")
                .icon(IconName::Square)
                .accessibility_label("Stop")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)))
        } else {
            InputGroupButton::new("send")
                .icon(IconName::ArrowUp)
                .accessibility_label("Send")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx)))
        };

        // The chat fills the window right of the sidebar.
        let chat = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(background)
            .child(
                // Opens up as the sidebar goes, for the traffic lights and the sidebar
                // button to sit in.
                div()
                    .flex_none()
                    .h(self.title_bar_height * hidden)
                    .when(title_bar, as_title_bar),
            )
            .child(
                MessageScroller::new(
                    // Keyed per session, so a switch never reuses another session's list state.
                    ElementId::Name(format!("messages-{}", session.key.0).into()),
                    session.scroller.clone(),
                    cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx)),
                )
                .flex_1()
                .min_h(px(0.)),
            )
            .child(
                // A row along the input box's bottom: the session's model and mode on the
                // left, the send/stop button on the right.
                div().p_3().child(
                    InputGroup::new("prompt")
                        .rounded_xl()
                        // The group brightens its border and rings itself while it has
                        // focus. An inset shadow draws its resting border instead, so the
                        // box looks the same focused or not.
                        .border_0()
                        .shadow(vec![
                            BoxShadow::new(px(0.), px(0.), input_border)
                                .spread_radius(px(1.))
                                .inset(),
                        ])
                        .focus_ring(false)
                        // With the 44px row below it, the box is 129px tall until the
                        // text needs more room, with the text at the top.
                        .input(
                            Textarea::new(&self.input)
                                .min_h(px(85.))
                                // On top of the textarea's own 10px and 8px, so the
                                // text starts 17px in from the left, level with the
                                // model button's label, and its capitals 17px down.
                                .px(px(6.))
                                .pt(px(4.)),
                        )
                        .addon(
                            InputGroupAddon::new("prompt-actions")
                                .align(InputGroupAddonAlignment::BlockEnd)
                                // 6px in from the box's sides and bottom, like the gap
                                // above the buttons.
                                .px_1p5()
                                .pb_1p5()
                                .children(pickers.into_iter().flatten())
                                .child(div().flex_1())
                                // A pointing hand over it, where the component's buttons
                                // keep the arrow.
                                .child(action.primary().small().rounded_full().cursor_pointer()),
                        ),
                ),
            );

        // Stays beside the traffic lights, over the sidebar or the chat, while the sidebar
        // slides.
        let toggle = div()
            .id("toggle-sidebar")
            .role(Role::Button)
            .aria_label(if self.sidebar_open {
                "Hide sidebar"
            } else {
                "Show sidebar"
            })
            .absolute()
            .top((self.title_bar_height - px(28.)) / 2.)
            // 6px past the zoom button leaves about 12px before the icon's outline.
            // Without the traffic lights, the outline lines up with the sidebar's rows.
            .left(if title_bar {
                self.traffic_lights_end + px(6.)
            } else {
                px(14.25)
            })
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(hover))
            // Keeps presses from the title bar strip underneath, which would move the
            // window instead.
            .occlude()
            // A little larger than the traffic lights, since an outline looks smaller
            // than a filled circle of the same size.
            .child(
                Icon::empty()
                    .data(SIDEBAR_ICON)
                    .size(px(18.))
                    .text_color(muted),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }));

        div()
            .size_full()
            .relative()
            .flex()
            .bg(sidebar)
            .text_color(foreground)
            // A drag of the sidebar's edge can leave the edge's own band within a pixel,
            // so the whole window follows it: the sidebar ends where the pointer is.
            // While the sidebar slides in its edge is short of its width, so a drag
            // caught mid-slide waits for the slide to end; this listener is made anew
            // on every frame of it.
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<ResizeSidebar>, _, cx| {
                    if hidden > 0. {
                        return;
                    }
                    let width = event.event.position.x - event.bounds.left();
                    this.sidebar_width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                    cx.notify();
                }),
            )
            // The prompt box keeps keyboard focus while text in the chat is selected, and
            // its own Copy handler would swallow cmd-c, so copy the chat's selection first.
            .capture_action(|_: &Copy, window, cx| {
                let text = TextSelection::selected_text(window, cx).trim().to_string();
                if text.is_empty() {
                    return;
                }
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
            })
            .child(
                // The sidebar keeps its width and slides out to the left as this narrows.
                div()
                    .flex_none()
                    .h_full()
                    .w(sidebar_width)
                    .overflow_hidden()
                    .child(
                        self.render_sidebar(title_bar, cx)
                            .ml(sidebar_width - self.sidebar_width),
                    ),
            )
            .child(chat)
            .child(toggle)
    }
}

/// `config` with the sidebar and the chat in two grays. In both modes the sidebar is the
/// darker one, by about the same small step in lightness.
fn with_panel_colors(
    config: &ThemeConfig,
    sidebar: &'static str,
    chat: &'static str,
) -> Rc<ThemeConfig> {
    let mut config = config.clone();
    config.colors.sidebar = Some(sidebar.into());
    config.colors.background = Some(chat.into());
    Rc::new(config)
}

/// The folder for the app's own files.
fn config_folder() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("acp-client"))
}

/// Holds the older models the user added to the model list, an id a line.
const ADDED_MODELS: &str = "added-models";

fn load_added_models() -> Vec<String> {
    config_folder()
        .and_then(|folder| read_added_models(&folder.join(ADDED_MODELS)).ok())
        .unwrap_or_default()
}

/// An error other than a missing file is passed on, so a list that couldn't be read
/// isn't written over.
fn read_added_models(path: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(Into::into)
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Makes `change` to the added models kept in `folder`, and returns the list as it then
/// stands. Windows changing it at once take turns under a lock, and the new list
/// replaces the old one whole, so a write that fails or is cut short leaves the old list.
fn change_added_models(
    folder: &Path,
    change: impl Fn(&mut Vec<String>),
) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(folder)?;
    // Held until the new list is in place.
    let lock = std::fs::File::create(folder.join(format!("{ADDED_MODELS}.lock")))?;
    lock.lock()?;
    let path = folder.join(ADDED_MODELS);
    let mut models = read_added_models(&path)?;
    change(&mut models);
    let written = folder.join(format!("{ADDED_MODELS}.new"));
    std::fs::write(
        &written,
        models
            .iter()
            .map(|id| format!("{id}\n"))
            .collect::<String>(),
    )?;
    std::fs::rename(&written, &path)?;
    Ok(models)
}

/// Holds the settings each agent last created a session with, by `kept_name`, as JSON.
const LAST_SETTINGS: &str = "last-settings";

/// The name an agent's settings are kept under on disk: its harness's, whether the
/// window started it as that or by the `claude` or `codex` argument. None for any other
/// command, which is all that names such an agent and may carry a secret, such as a key.
fn kept_name(name: &str) -> Option<&'static str> {
    Harness::ALL
        .into_iter()
        .map(Harness::name)
        .find(|kept| kept.eq_ignore_ascii_case(name))
}

/// A file that is missing or can't be read gives none, and the buttons wait for the
/// agent as they did before there was one.
fn load_last_settings() -> HashMap<String, Vec<Setting>> {
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
fn save_last_settings(folder: &Path, agent: &str, settings: &[Setting]) -> std::io::Result<()> {
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

// The icons `mode_icon` uses beyond those in the component library's own bundle.
gpui_kit::assets::icon_assets!(
    ModeIcons,
    [
        Hand,
        FilePen,
        ListChecks,
        FolderPen,
        ShieldCheck,
        ShieldOff,
        Shield
    ]
);

/// The component library's icons, and the mode icons.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ModeIcons.load(path)? {
            Some(icon) => Ok(Some(icon)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ModeIcons.list(path)?);
        Ok(paths)
    }
}

actions!(acp_gui, [Quit]);

fn main() {
    // One agent by name, or both harnesses to pick between from the model menu.
    let name = std::env::args().nth(1);
    let agents: Vec<(String, AcpAgent)> = match &name {
        Some(name) => match acp_core::agent_for(name) {
            Ok(agent) => vec![(name.clone(), agent)],
            Err(err) => {
                eprintln!("bad agent command: {err}");
                std::process::exit(2);
            }
        },
        None => Harness::ALL
            .into_iter()
            .map(|harness| (harness.name().to_string(), harness.agent()))
            .collect(),
    };
    let title = match &name {
        Some(name) => format!("ACP client: {name}"),
        None => "ACP client".to_string(),
    };
    let cwd = std::env::current_dir().expect("no current directory");

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            // Nothing quits on Cmd+Q unless the app says so: gpui-kit leaves the shortcut
            // to the app, and with no menus set AppKit has no Quit item to match it to.
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus([
                Menu::new("ACP client").items([MenuItem::action("Quit ACP client", Quit)])
            ]);
            Theme::update(cx, |theme| {
                theme.light_theme = with_panel_colors(&theme.light_theme, "#f7f7f7", "#fcfcfc");
                theme.dark_theme = with_panel_colors(&theme.dark_theme, "#111111", "#151515");
            });
            let lights = TrafficLights::new(traffic_light_size());
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1000.), px(820.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    appears_transparent: OWN_TITLE_BAR,
                    traffic_light_position: OWN_TITLE_BAR.then_some(lights.origin),
                }),
                // The app moves the window itself (see `as_title_bar`). Otherwise AppKit
                // does it from the strip around the traffic lights, and on macOS 27 it
                // holds back every click there while it waits for a double-click.
                app_owns_titlebar_drag: OWN_TITLE_BAR,
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                // Light or dark as the system is set, and again whenever that setting changes.
                Theme::sync_system_appearance(Some(window), cx);
                window
                    .observe_window_appearance(|window, cx| {
                        Theme::sync_system_appearance(Some(window), cx)
                    })
                    .detach();
                let lights_end = lights.end(window);
                cx.new(|cx| ChatView::new(agents, cwd, lights.row_height, lights_end, window, cx))
            })
            .expect("failed to open the window");
            cx.activate(true);
        });
}
