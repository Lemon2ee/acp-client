//! Minimal gpui front end: a sidebar of sessions beside a chat, driving ACP agents.
//! Usage: `acp-gui [claude | codex | "<agent command>"]`. With no argument it runs
//! both Claude and Codex, and the model menu picks between them.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use acp_core::{
    AcpAgent, Command, Event as AgentEvent, Harness, PermissionRequest, Sender, SessionEvent,
    SessionKey, Setting, SettingChoice, SettingKind, SettingValue,
};
use gpui_kit::base::input::Copy;
use gpui_kit::base::{HandleEdge, SelectableText, TextSelection, Transition, transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Input, InputEvent, InputGroup, InputGroupAddon, InputGroupAddonAlignment, InputGroupButton,
    InputState, Textarea, TextareaState,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, FocusableExt as _, Icon, IconName, Sizable as _, Theme,
    ThemeConfig, ThemeStyled as _, h_flex, resize_handle, v_flex,
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
    /// Not ready for a prompt: still starting, a turn is running, or the agent is gone.
    busy: bool,
    /// Text typed here but not sent, kept while another session is on screen.
    draft: SharedString,
    /// What the agent lets the user choose for this session, such as its model; none
    /// until the agent has created it.
    settings: Vec<Setting>,
    /// The agent has created it and given its settings, if any.
    created: bool,
    /// Switches flipped here whose changes the agent hasn't all answered, by setting id.
    flipped: HashMap<String, Flip>,
}

/// A switch flipped here, until the agent has answered every change it was asked for.
struct Flip {
    /// The value last asked for, which the switch shows meanwhile.
    on: bool,
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
            busy: true,
            draft: SharedString::default(),
            settings: Vec::new(),
            created: false,
            flipped: HashMap::new(),
        }
    }

    /// A setting as it shows: a switch flipped here shows the value last asked for until
    /// the agent has answered all its changes.
    fn shown(&self, setting: &Setting) -> Setting {
        let mut setting = setting.clone();
        if let Some(flip) = self.flipped.get(&setting.id) {
            setting.value = SettingValue::Switch(flip.on);
        }
        setting
    }

    /// One of a setting's changes has its answer. A switch shows the agent's value
    /// again once all of its have, as an answer to an earlier flip can arrive while a
    /// later one is still out.
    fn answered(&mut self, setting: &str) {
        if let Some(flip) = self.flipped.get_mut(setting) {
            // A late answer to a change already given up on finds nothing to count.
            flip.out = flip.out.saturating_sub(1);
            if flip.out == 0 {
                self.flipped.remove(setting);
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
    /// A session created ahead of its use while the agent has no session on show, so
    /// its models can be listed from sessions on other agents, and so the first
    /// session on it is shown with its settings at once. Taken by that session.
    spare: Option<Session>,
    /// The spare couldn't be created, and the user has been told; said once, as each
    /// session added tries the spare again.
    spare_failed: bool,
    /// The settings the agent last created a session with, in this run or, for Claude
    /// and Codex, one before, which a session on it shows while the agent creates it.
    /// Creating one takes the agent seconds: Claude's adapter starts the Claude Code
    /// CLI for it.
    last_settings: Vec<Setting>,
    _events: Task<()>,
}

struct ChatView {
    /// Never empty: the window starts with one session and none are removed.
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
    model_panel: ModelPanelState,
    /// The models the user starred, which the model panel's favorites tab lists.
    /// Kept on disk.
    favorites: Vec<Favorite>,
    _input_events: Subscription,
    _search_events: Subscription,
}

impl ChatView {
    /// Starts every agent in `agents`, each by name, and opens a session on the first.
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
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let search_events = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
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
            model_panel: ModelPanelState {
                tab: ModelTab::Favorites,
                search,
            },
            favorites: load_favorites(),
            _input_events: input_events,
            _search_events: search_events,
        };
        view.new_session(window, cx);
        view
    }

    /// Asks the agent to create a session, and returns it as it waits to be created.
    /// With the agent gone, the session says so and stays closed to prompts.
    fn create_session(&mut self, agent: usize, cx: &mut App) -> Session {
        let key = SessionKey(self.next_key);
        self.next_key += 1;
        let mut session = Session::new(key, agent, cx);
        let asked = self.agents[agent]
            .commands
            .try_send(Command::NewSession(key));
        if asked.is_err() {
            let name = &self.agents[agent].name;
            session.push(
                Row::Error(format!("{name} has gone; restart to use it")),
                cx,
            );
        }
        session
    }

    /// Makes a spare session for each agent that has no session on show and no spare,
    /// and is still there to create it. A session on show lists the agent's models
    /// just as well, so an agent with one keeps no spare.
    fn make_spares(&mut self, cx: &mut App) {
        for agent in 0..self.agents.len() {
            let slot = &self.agents[agent];
            if slot.spare.is_some()
                || slot.commands.is_closed()
                || self.sessions.iter().any(|session| session.agent == agent)
            {
                continue;
            }
            let spare = self.create_session(agent, cx);
            self.agents[agent].spare = Some(spare);
        }
    }

    /// A session on the agent, to show: its spare, ready with its settings, or a new one
    /// when it has none.
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

    /// Adds a session on the agent of the one on screen, and shows it. The window's
    /// first session is added even if the agent is already gone, so there is always
    /// one to show.
    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self
            .sessions
            .get(self.active)
            .map_or(0, |session| session.agent);
        if self.agents[agent].commands.is_closed() && !self.sessions.is_empty() {
            return;
        }
        let session = self.take_session(agent, cx);
        self.sessions.push(session);
        // Also another go for any agent whose spare failed to be created.
        self.make_spares(cx);
        self.show(self.sessions.len() - 1, window, cx);
    }

    /// Picks a model of another agent's from the model menu of the session `from`. A
    /// session with no prompt yet is replaced by one on that agent, as ACP can't move
    /// it there; one with prompts keeps its agent, and a new session on the other
    /// agent is shown instead. Nothing happens for an agent that is gone.
    fn pick_model(
        &mut self,
        from: SessionKey,
        agent: usize,
        setting: String,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.sessions.iter().position(|session| session.key == from) else {
            return;
        };
        if self.agents[agent].commands.is_closed() {
            return;
        }
        let session = self.take_session(agent, cx);
        // acp-core holds the change until the session exists, and a prompt waits for it.
        let value = SettingValue::Choice(model);
        let change = Command::ChangeSetting(session.key, setting, value);
        let _ = self.agents[agent].commands.try_send(change);
        let shown = if self.sessions[ix].title.is_none() {
            let old = std::mem::replace(&mut self.sessions[ix], session);
            let _ = self.agents[old.agent]
                .commands
                .try_send(Command::CloseSession(old.key));
            ix
        } else {
            self.sessions.push(session);
            self.sessions.len() - 1
        };
        // The agent left behind may have no session on show now.
        self.make_spares(cx);
        self.show(shown, window, cx);
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
        let prompt = Command::Prompt(session.key, text);
        let _ = self.agents[session.agent].commands.try_send(prompt);
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

    /// The setting shows its old value until the agent confirms the change.
    fn change_setting(&mut self, key: SessionKey, setting: String, value: SettingValue) {
        let Some(agent) = self.session_mut(key).map(|session| session.agent) else {
            return;
        };
        let change = Command::ChangeSetting(key, setting, value);
        let _ = self.agents[agent].commands.try_send(change);
    }

    /// Flips one of the session's switches from the value it shows, which is the value
    /// last asked for until the agent answers, so a second flip before then turns it
    /// back. Unlike a pick from a list, the switch shows its new value at once.
    fn flip_switch(&mut self, key: SessionKey, setting: String, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut(key) else {
            return;
        };
        let agent = session.agent;
        let shown = session
            .settings
            .iter()
            .find(|shown| shown.id == setting)
            .map(|shown| session.shown(shown).value);
        let Some(SettingValue::Switch(on)) = shown else {
            return;
        };
        let change = Command::ChangeSetting(key, setting.clone(), SettingValue::Switch(!on));
        // With the agent gone, no answer would ever put the switch right.
        if self.agents[agent].commands.try_send(change).is_err() {
            return;
        }
        if let Some(session) = self.session_mut(key) {
            let flip = session
                .flipped
                .entry(setting)
                .or_insert(Flip { on, out: 0 });
            flip.on = !on;
            flip.out += 1;
        }
        cx.notify();
    }

    /// Stars a model, or takes its star off again. Another window may have changed the
    /// favorites since this one read them, so the change is made to the list on disk,
    /// and this window takes that list up.
    fn set_favorite(&mut self, favorite: Favorite, starred: bool, cx: &mut Context<Self>) {
        let change = |favorites: &mut Vec<Favorite>| {
            favorites.retain(|known| *known != favorite);
            if starred {
                favorites.push(favorite.clone());
            }
        };
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| change_favorites(&folder, change));
        match changed {
            Ok(favorites) => self.favorites = favorites,
            Err(err) => {
                eprintln!("couldn't save the favorite models: {err}");
                // The list still changes for this run.
                change(&mut self.favorites);
            }
        }
        cx.notify();
    }

    /// Shows `tab` of the model panel.
    fn show_model_tab(&mut self, tab: ModelTab, cx: &mut Context<Self>) {
        self.model_panel.tab = tab;
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
                    session.flipped.clear();
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

/// How tall the model panel's switch rows are, and the gap between rows.
const MENU_ROW_HEIGHT: Pixels = px(36.);
const MENU_ROW_GAP: Pixels = px(2.);
/// How tall a model's row in the panel is.
const LIST_ROW_HEIGHT: Pixels = px(32.);
/// How tall the line naming an agent above its models is, in the favorites.
const LIST_HEADER_HEIGHT: Pixels = px(24.);
/// The model panel's size. Its models scroll inside it, so a tab with few models is
/// as tall as one with many.
const PANEL_WIDTH: Pixels = px(340.);
const PANEL_HEIGHT: Pixels = px(420.);
/// How wide the panel's column of tabs is, and how big each tab is.
const TAB_COLUMN_WIDTH: Pixels = px(56.);
const TAB_SIZE: Pixels = px(40.);
/// How tall the search box's row along the top of the panel is.
const SEARCH_ROW_HEIGHT: Pixels = px(44.);
/// Groups a model's row with its star, which shows while the row is pointed at.
const MODEL_ROW_GROUP: &str = "model-panel-row";

/// The harnesses' marks, for the panel's tabs.
const CLAUDE_LOGO: &[u8] = include_bytes!("../assets/claude.svg");
const OPENAI_LOGO: &[u8] = include_bytes!("../assets/openai.svg");
/// The orange Claude's mark is drawn in, in both light and dark mode.
const CLAUDE_ORANGE: u32 = 0xD97757;

/// The agent's mark for its tab: its harness's, or a terminal for a command.
fn agent_logo(name: &str) -> Icon {
    if name == Harness::Claude.name() {
        Icon::empty()
            .data(CLAUDE_LOGO)
            .text_color(rgb(CLAUDE_ORANGE))
    } else if name == Harness::Codex.name() {
        Icon::empty().data(OPENAI_LOGO)
    } else {
        Icon::new(IconName::SquareTerminal)
    }
}

/// Whether the choice is an adapter's "default" entry, which stands for one of the
/// other choices and isn't listed.
fn is_default(choice: &SettingChoice) -> bool {
    choice.id == "default"
}

/// The choice a setting shows as its value. An adapter can set a setting to its
/// "default" entry, which isn't listed; its description names the choice it stands for.
fn shown_choice(setting: &Setting) -> Option<&SettingChoice> {
    let chosen = setting.chosen()?;
    if !is_default(chosen) {
        return Some(chosen);
    }
    let stands_for = chosen.description.as_deref();
    let choice = setting
        .choices
        .iter()
        .find(|choice| !is_default(choice) && Some(choice.name.as_str()) == stands_for);
    Some(choice.unwrap_or(chosen))
}

/// What the model panel keeps while it is open.
struct ModelPanelState {
    /// The tab on show.
    tab: ModelTab,
    /// The search box, which narrows the tab to the models named like its text.
    search: Entity<InputState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelTab {
    /// The starred models of every agent.
    Favorites,
    /// The models of the agent at this index.
    Agent(usize),
}

/// A model the user starred: its agent's name and its id. The agent goes by name, not
/// index, so the star holds across runs that start the agents in another order.
#[derive(Clone, PartialEq, Eq)]
struct Favorite {
    agent: String,
    model: String,
}

/// One agent's models in the model panel.
struct ModelGroup {
    agent: usize,
    name: String,
    setting: Setting,
    /// Whether this is the session's own agent, whose model is set in place. Another
    /// agent's model goes on to a session on that agent.
    own: bool,
}

/// What the parts of the model panel share.
struct ModelPanel<'a> {
    view: &'a WeakEntity<ChatView>,
    key: SessionKey,
    popover: &'a Entity<PopoverState>,
    search: &'a Entity<InputState>,
    accent: Hsla,
    accent_foreground: Hsla,
    muted: Hsla,
    /// How the component library's menus round their rows.
    radius: Pixels,
}

impl ModelPanel<'_> {
    /// A tab in the panel's left column, which shows `tab` when pressed.
    fn tab(
        &self,
        tab: ModelTab,
        icon: Icon,
        name: SharedString,
        selected: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let view = self.view.clone();
        let (selected_bg, foreground) = (cx.theme().secondary, cx.theme().foreground);
        let tooltip = name.clone();
        div()
            .id(ElementId::Name(format!("model-tab-{name}").into()))
            .size(TAB_SIZE)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(self.radius)
            .cursor_pointer()
            .text_color(self.muted)
            .hover(move |style| style.bg(selected_bg).text_color(foreground))
            .when(selected, |tab| tab.bg(selected_bg).text_color(foreground))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(icon.size(px(20.)))
            .on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.show_model_tab(tab, cx));
            })
    }

    /// A model's row: its name, its star, and a check after the session's model. A
    /// press sets the session's model, or goes on to a session on the model's agent,
    /// and closes the panel.
    fn model_row(
        &self,
        group: &ModelGroup,
        choice: &SettingChoice,
        current: bool,
        starred: bool,
    ) -> Stateful<Div> {
        let (view, popover, key) = (self.view.clone(), self.popover.clone(), self.key);
        let (agent, own) = (group.agent, group.own);
        let (setting, model) = (group.setting.id.clone(), choice.id.clone());
        let (accent, accent_foreground) = (self.accent, self.accent_foreground);
        div()
            .id(ElementId::Name(
                format!("model-row-{agent}-{}", choice.id).into(),
            ))
            .group(MODEL_ROW_GROUP)
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
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(choice.name.clone())),
            )
            .child(self.star(group, choice, starred))
            .when(current, |row| {
                row.child(Icon::new(IconName::Check).xsmall())
            })
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    if own {
                        let value = SettingValue::Choice(model.clone());
                        this.change_setting(key, setting.clone(), value);
                    } else {
                        this.pick_model(key, agent, setting.clone(), model.clone(), window, cx);
                    }
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
    }

    /// The star on a model's row, which stars the model or takes its star off. A
    /// starred model's star is filled and always shows; the others show while their
    /// row is pointed at.
    fn star(&self, group: &ModelGroup, choice: &SettingChoice, starred: bool) -> AnyElement {
        let view = self.view.clone();
        let favorite = Favorite {
            agent: group.name.clone(),
            model: choice.id.clone(),
        };
        div()
            .flex_none()
            // The button draws its icon 4px in from its edge. This puts the icon against
            // the row's padding, where a row's check goes.
            .mr(px(-4.))
            .when(!starred, |star| {
                star.opacity(0.)
                    .group_hover(MODEL_ROW_GROUP, |style| style.opacity(1.))
            })
            .child(
                Button::new(ElementId::Name(
                    format!("model-star-{}-{}", group.agent, choice.id).into(),
                ))
                .ghost()
                .xsmall()
                .icon(if starred {
                    IconName::StarFill
                } else {
                    IconName::Star
                })
                .tooltip(if starred {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                })
                .on_click(move |_, _, cx| {
                    // Doesn't pick the model, as a press elsewhere on its row does.
                    cx.stop_propagation();
                    let _ = view.update(cx, |this, cx| {
                        this.set_favorite(favorite.clone(), !starred, cx)
                    });
                }),
            )
            .into_any_element()
    }

    /// A line in the favorites naming the agent whose models follow it.
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

    /// A row along the panel's bottom for a setting of the model that is on or off.
    /// The switch only shows the value: a press anywhere on the row flips it.
    fn switch_row(&self, setting: &Setting, on: bool) -> Stateful<Div> {
        let (view, key, id) = (self.view.clone(), self.key, setting.id.clone());
        let (accent, accent_foreground) = (self.accent, self.accent_foreground);
        div()
            .id(ElementId::Name(
                format!("model-switch-{}", setting.id).into(),
            ))
            .h(MENU_ROW_HEIGHT)
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
                    .child(SharedString::from(setting.name.clone())),
            )
            .child(
                Switch::new(ElementId::Name(
                    format!("model-switch-value-{}", setting.id).into(),
                ))
                .checked(on)
                .tab_stop(false),
            )
            .on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.flip_switch(key, id.clone(), cx));
            })
    }
}

/// `surface`, a panel with the popover style, fading in on `animation` as it moves the
/// last of the way into place: `place` puts it `rest` of the way back, from 1 down to 0.
/// GPUI has no transform for a whole panel, so it moves by its offsets, and fades each
/// thing it paints alone rather than the panel as one, so a half-faded panel would show
/// its own shadow through it. The shadow comes in by the cube of the fade instead, out
/// of sight until the panel covers it, as the component library's dropdowns do. With
/// reduced motion on, GPUI draws the panel in place at once.
fn come_in<E: Styled + IntoElement + 'static>(
    surface: E,
    id: impl Into<ElementId>,
    animation: Animation,
    place: impl Fn(E, f32) -> E + 'static,
) -> AnimationElement<E> {
    surface.with_animation(id, animation, move |mut surface, delta| {
        for shadow in surface.style().box_shadow.iter_mut().flatten() {
            shadow.color.a *= delta.powi(3);
        }
        place(surface, 1. - delta).opacity(delta)
    })
}

/// The models of `group` named like `query`, with whether each is the session's model.
fn listed_models<'a>(group: &'a ModelGroup, query: &str) -> Vec<(&'a SettingChoice, bool)> {
    let current = group
        .own
        .then(|| shown_choice(&group.setting))
        .flatten()
        .map(|choice| choice.id.as_str());
    group
        .setting
        .choices
        .iter()
        .filter(|choice| !is_default(choice))
        .filter(|choice| query.is_empty() || choice.name.to_lowercase().contains(query))
        .map(|choice| (choice, current == Some(choice.id.as_str())))
        .collect()
}

/// The model panel: a column of tabs on the left, the favorites and then one for each
/// agent, and on the right a search box over the tab's models. The session's settings
/// of the model that are on or off, such as a fast mode, are switches along the bottom.
fn model_panel(
    panel: &ModelPanel,
    groups: &[ModelGroup],
    switches: &[Setting],
    tab: ModelTab,
    favorites: &[Favorite],
    window: &Window,
    cx: &App,
) -> AnimationElement<Div> {
    let (border, muted) = (cx.theme().border, panel.muted);
    let query = panel.search.read(cx).value().trim().to_lowercase();
    let starred = |group: &ModelGroup, choice: &SettingChoice| {
        favorites
            .iter()
            .any(|favorite| favorite.agent == group.name && favorite.model == choice.id)
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    let empty =
        match tab {
            ModelTab::Favorites => {
                // Each agent's starred models under its name.
                for group in groups {
                    let starred: Vec<_> = listed_models(group, &query)
                        .into_iter()
                        .filter(|(choice, _)| starred(group, choice))
                        .collect();
                    if starred.is_empty() {
                        continue;
                    }
                    rows.push(panel.group_header(&group.name));
                    rows.extend(starred.into_iter().map(|(choice, current)| {
                        panel
                            .model_row(group, choice, current, true)
                            .into_any_element()
                    }));
                }
                if query.is_empty() {
                    "No favorites yet. Star a model to keep it here."
                } else {
                    "No favorites match."
                }
            }
            ModelTab::Agent(agent) => {
                for group in groups.iter().filter(|group| group.agent == agent) {
                    rows.extend(listed_models(group, &query).into_iter().map(
                        |(choice, current)| {
                            panel
                                .model_row(group, choice, current, starred(group, choice))
                                .into_any_element()
                        },
                    ));
                }
                "No models match."
            }
        };

    let tabs = div()
        .w(TAB_COLUMN_WIDTH)
        .flex_none()
        .py_2()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .border_r_1()
        .border_color(border)
        .child(panel.tab(
            ModelTab::Favorites,
            Icon::new(IconName::Star),
            "Favorites".into(),
            tab == ModelTab::Favorites,
            cx,
        ))
        .children(groups.iter().map(|group| {
            panel.tab(
                ModelTab::Agent(group.agent),
                agent_logo(&group.name),
                group.name.clone().into(),
                tab == ModelTab::Agent(group.agent),
                cx,
            )
        }));
    let search = div()
        .h(SEARCH_ROW_HEIGHT)
        .flex_none()
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(border)
        .child(Icon::new(IconName::Search).size(px(16.)).text_color(muted))
        .child(
            Input::new(panel.search)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .small()
                .flex_1(),
        );
    let list = if rows.is_empty() {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .text_xs()
            .text_color(muted)
            .whitespace_normal()
            .text_center()
            .child(empty)
            .into_any_element()
    } else {
        div()
            .id("model-panel-rows")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p_1()
            .flex()
            .flex_col()
            .gap(MENU_ROW_GAP)
            .children(rows)
            .into_any_element()
    };
    let footer = (!switches.is_empty()).then(|| {
        div()
            .flex_none()
            .p_1()
            .border_t_1()
            .border_color(border)
            .flex()
            .flex_col()
            .gap(MENU_ROW_GAP)
            .children(switches.iter().map(|setting| {
                let on = matches!(setting.value, SettingValue::Switch(true));
                panel.switch_row(setting, on)
            }))
    });

    let surface = div()
        .relative()
        .w(PANEL_WIDTH)
        .h(PANEL_HEIGHT)
        .max_h(window.viewport_size().height * 0.8)
        .popover_style(cx)
        .overflow_hidden()
        .flex()
        .text_sm()
        // Rows are one line tall; a long name is cut short instead of wrapping.
        .whitespace_nowrap()
        .child(tabs)
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(search)
                .child(list)
                .children(footer),
        );
    // It opens upward, so it rises out of the button below it as it fades in. The
    // popover is drawn only while open, so each open plays this afresh.
    let motion = cx.theme().motion_tokens();
    let travel = motion.distance_short.to_pixels(window.rem_size());
    let easing = motion.easing_enter.clone();
    let enter = Animation::new(motion.duration_normal).with_easing(move |t| easing.sample(t));
    come_in(surface, "model-panel-in", enter, move |surface, rest| {
        surface.top(travel * rest)
    })
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

/// The kinds of the model's settings the model button stands for: the model, and
/// the switches along the bottom of its panel.
const MODEL_KINDS: [SettingKind; 2] = [SettingKind::Model, SettingKind::ModelConfig];

/// The model's button, named for the model. An agent that offers no model still gets
/// the button, for the other agents' models and its switches.
fn model_button(settings: &[Setting]) -> InputGroupButton {
    let label = settings
        .iter()
        .find(|setting| setting.kind == SettingKind::Model)
        .and_then(shown_choice)
        .map_or_else(|| "Model".to_string(), |choice| choice.name.clone());
    InputGroupButton::new("model-picker")
        .label(label.clone())
        .accessibility_label(format!("Model: {label}"))
        .dropdown_caret(true)
        .small()
}

/// The button for how hard the model thinks, named for the level.
fn effort_button(setting: &Setting) -> InputGroupButton {
    let name =
        shown_choice(setting).map_or_else(|| setting.name.clone(), |choice| choice.name.clone());
    InputGroupButton::new("effort-picker")
        .label(name.clone())
        .accessibility_label(format!("{}: {name}", setting.name))
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

/// The model's, the effort's and the mode's buttons as `settings` have them, dimmed and
/// without menus, for a session the agent is still creating: until it has, there is
/// nothing to change. They stand where the session's own buttons will, so nothing
/// moves when those come.
fn starting_pickers(settings: &[Setting]) -> [Option<AnyElement>; 3] {
    let model = settings
        .iter()
        .any(|setting| MODEL_KINDS.contains(&setting.kind))
        .then(|| model_button(settings).disabled(true).into_any_element());
    let effort = settings
        .iter()
        .find(|setting| setting.kind == SettingKind::ThoughtLevel)
        .map(|setting| effort_button(setting).disabled(true).into_any_element());
    let mode = settings
        .iter()
        .find(|setting| setting.kind == SettingKind::Mode)
        .map(|setting| mode_button(setting).disabled(true).into_any_element());
    [model, effort, mode]
}

impl ChatView {
    /// The model's button, named for the session's model, and the panel of models that
    /// opens upward from it: the favorites, the session's own agent's models, and the
    /// other agents'.
    fn render_model_picker(&self, session: &Session, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Each agent's models, in the agents' order, once a session on it has its
        // settings. An agent that is gone can't take a session, so its models aren't
        // offered; the session's own agent's are, as they are the session's.
        let groups: Vec<ModelGroup> = self
            .agents
            .iter()
            .enumerate()
            .filter_map(|(ix, slot)| {
                let own = ix == session.agent;
                if !own && slot.commands.is_closed() {
                    return None;
                }
                let source = if own { session } else { self.model_source(ix)? };
                let setting = source
                    .settings
                    .iter()
                    .find(|setting| setting.kind == SettingKind::Model)?;
                Some(ModelGroup {
                    agent: ix,
                    name: slot.name.clone(),
                    setting: source.shown(setting),
                    own,
                })
            })
            .collect();
        let switches: Vec<Setting> = session
            .settings
            .iter()
            .filter(|setting| setting.kind == SettingKind::ModelConfig)
            .map(|setting| session.shown(setting))
            .collect();
        if groups.is_empty() && switches.is_empty() {
            return None;
        }
        let own = groups.iter().find(|group| group.own);
        // The panel opens on the session's own agent's tab, so its model is in view, or
        // on the first agent's when the session has no model to pick.
        let first_tab = own
            .or(groups.first())
            .map_or(ModelTab::Favorites, |group| ModelTab::Agent(group.agent));
        let view = cx.entity().downgrade();
        let key = session.key;
        let tab = self.model_panel.tab;
        let search = self.model_panel.search.clone();
        let favorites = self.favorites.clone();
        let trigger = model_button(&session.settings).cursor_pointer();
        let picker = Popover::new("model-panel")
            .appearance(false)
            .anchor(Anchor::BottomLeft)
            .trigger(trigger)
            // Opening the panel focuses the search box, so typing narrows the list at once.
            .track_focus(&search.read(cx).focus_handle(cx))
            .on_open_change({
                let (view, search) = (view.clone(), search.clone());
                move |open, window, cx| {
                    if !*open {
                        return;
                    }
                    // It opens on `first_tab` with an empty search, each time.
                    search.update(cx, |search, cx| search.set_value(String::new(), window, cx));
                    let _ = view.update(cx, |this, cx| this.show_model_tab(first_tab, cx));
                }
            })
            .content(move |_, window, cx| {
                let popover = cx.entity();
                let theme = cx.theme();
                let panel = ModelPanel {
                    view: &view,
                    key,
                    popover: &popover,
                    search: &search,
                    accent: *theme.tokens.accent,
                    accent_foreground: theme.accent_foreground,
                    muted: theme.muted_foreground,
                    radius: theme.radius.min(px(8.)),
                };
                model_panel(&panel, &groups, &switches, tab, &favorites, window, cx)
            });
        Some(picker.into_any_element())
    }

    /// The button for how hard the model thinks, and a menu of the levels that opens
    /// upward. The adapter's "default" level is left out: it stands for one of the others.
    fn render_effort_picker(
        &self,
        key: SessionKey,
        setting: &Setting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let setting = setting.clone();
        effort_button(&setting)
            .cursor_pointer()
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                let current = shown_choice(&setting).map(|choice| choice.id.clone());
                setting
                    .choices
                    .iter()
                    .filter(|choice| !is_default(choice))
                    .fold(menu, |menu, choice| {
                        let checked = current.as_deref() == Some(choice.id.as_str());
                        let (view, id, value) =
                            (view.clone(), setting.id.clone(), choice.id.clone());
                        menu.item(
                            PopupMenuItem::new(choice.name.clone())
                                .checked(checked)
                                .on_click(move |_, _, cx| {
                                    let _ = view.update(cx, |this, _| {
                                        this.change_setting(
                                            key,
                                            id.clone(),
                                            SettingValue::Choice(value.clone()),
                                        )
                                    });
                                }),
                        )
                    })
            })
            .into_any_element()
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
                        let _ = view.update(cx, |this, _| {
                            this.change_setting(
                                key,
                                id.clone(),
                                SettingValue::Choice(value.clone()),
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
        // Newest first; the session on screen gets a filled card.
        let cards =
            self.sessions.iter().enumerate().rev().map(|(ix, session)| {
                let key = session.key;
                let title =
                    SharedString::from(session.title.clone().unwrap_or_else(|| "Untitled".into()));
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
        let slot = &self.agents[session.agent];
        // The model, how hard it thinks, then the mode, each once the agent has said
        // what it can be. Until then, as the agent's last new session had them.
        let starting = !session.created && session.busy && !slot.commands.is_closed();
        let pickers = if starting && !slot.last_settings.is_empty() {
            starting_pickers(&slot.last_settings)
        } else {
            [
                self.render_model_picker(session, cx),
                session
                    .settings
                    .iter()
                    .find(|setting| setting.kind == SettingKind::ThoughtLevel)
                    .map(|setting| {
                        self.render_effort_picker(session.key, &session.shown(setting), cx)
                    }),
                session
                    .settings
                    .iter()
                    .find(|setting| setting.kind == SettingKind::Mode)
                    .map(|setting| self.render_mode_picker(session.key, setting, cx)),
            ]
        };
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

/// Holds the models the user starred, one a line: the agent's name, a tab, the model's id.
const FAVORITES: &str = "favorite-models";

fn load_favorites() -> Vec<Favorite> {
    config_folder()
        .and_then(|folder| read_favorites(&folder.join(FAVORITES)).ok())
        .unwrap_or_default()
}

/// An error other than a missing file is passed on, so a list that couldn't be read
/// isn't written over.
fn read_favorites(path: &Path) -> std::io::Result<Vec<Favorite>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(agent, model)| Favorite {
                agent: agent.into(),
                model: model.into(),
            })
            .collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Makes `change` to the favorites kept in `folder`, and returns the list as it then
/// stands. Windows changing it at once take turns under a lock, and the new list
/// replaces the old one whole, so a write that fails or is cut short leaves the old list.
fn change_favorites(
    folder: &Path,
    change: impl Fn(&mut Vec<Favorite>),
) -> std::io::Result<Vec<Favorite>> {
    std::fs::create_dir_all(folder)?;
    // Held until the new list is in place.
    let lock = std::fs::File::create(folder.join(format!("{FAVORITES}.lock")))?;
    lock.lock()?;
    let path = folder.join(FAVORITES);
    let mut favorites = read_favorites(&path)?;
    change(&mut favorites);
    let written = folder.join(format!("{FAVORITES}.new"));
    std::fs::write(
        &written,
        favorites
            .iter()
            .map(|favorite| format!("{}\t{}\n", favorite.agent, favorite.model))
            .collect::<String>(),
    )?;
    std::fs::rename(&written, &path)?;
    Ok(favorites)
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
