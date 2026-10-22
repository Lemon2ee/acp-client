//! Minimal gpui front end: a sidebar of sessions beside a chat, driving ACP agents.
//! Usage: `acp-gui [claude | codex | "<agent command>"]`. With no argument it runs
//! both Claude and Codex, and the model menu picks between them.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use acp_core::{
    AcpAgent, Command, Event as AgentEvent, Harness, PermissionRequest, Sender, SessionEvent,
    SessionKey, Setting, SettingChoice, SettingKind, SettingValue,
};
use gpui_kit::base::input::Copy;
use gpui_kit::base::{HandleEdge, SelectableText, TextSelection, Transition, transition};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
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
    ActiveTheme as _, FocusableExt as _, Icon, IconName, Sizable as _, Theme, ThemeConfig, h_flex,
    resize_handle, v_flex,
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
    /// The window's top row, across the sidebar and the chat. It holds the traffic lights
    /// on macOS, the sidebar button, and the name of the session on screen.
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
                    // Every session the agent creates, by a prompt or as a spare, gets
                    // its settings, so the agent creates sessions again and a spare
                    // failing after this is news.
                    self.agents[agent].spare_failed = false;
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
/// The sidebar button's width and height.
const SIDEBAR_BUTTON_SIZE: Pixels = px(28.);

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

/// The text of the model panel, the effort menu and the buttons that open them, and of
/// the small headings in them. 13px text on 28px rows is the density of macOS's menus.
const MENU_TEXT_SIZE: Pixels = px(13.);
const MENU_LINE_HEIGHT: Pixels = px(16.);
const MENU_HEADING_SIZE: Pixels = px(11.);
/// How tall a model's or an effort level's row is, a switch's row, and the line naming
/// an agent above its models in the favorites, which gets more room above it after the
/// first agent's.
const LIST_ROW_HEIGHT: Pixels = px(28.);
const SWITCH_ROW_HEIGHT: Pixels = px(30.);
const LIST_HEADER_HEIGHT: Pixels = px(26.);
const LIST_HEADER_GAP: Pixels = px(6.);
/// The heading over the effort menu's levels.
const MENU_HEADING_HEIGHT: Pixels = px(24.);
/// A row's padding. The right is narrower, as the star's button has room of its own.
const ROW_PAD_LEFT: Pixels = px(6.);
const ROW_PAD_RIGHT: Pixels = px(4.);
/// The column every row starts with, for a check, an agent's mark or the magnifier, so
/// that names, headings and the search text all start on one line, 32px into the list.
const LEAD_WIDTH: Pixels = px(16.);
const LEAD_ICON_SIZE: Pixels = px(14.);
const LEAD_GAP: Pixels = px(6.);
/// The agent's mark beside its name in the favorites.
const HEADING_ICON_SIZE: Pixels = px(12.);
/// The model panel's size. Its models scroll inside it, so a tab with few models is
/// as tall as one with many.
const PANEL_WIDTH: Pixels = px(320.);
const PANEL_HEIGHT: Pixels = px(380.);
/// How wide the effort menu is.
const EFFORT_MENU_WIDTH: Pixels = px(196.);
/// How round the panel and the effort menu are, and the rows and tabs in them. The rows
/// sit 4px in from the edge, so their corners follow the surface's.
const SURFACE_RADIUS: Pixels = px(10.);
const ROW_RADIUS: Pixels = px(6.);
const SURFACE_PADDING: Pixels = px(4.);
/// How far above its button the panel or the effort menu opens.
const MENU_OFFSET: Pixels = px(6.);
/// How wide the panel's column of tabs is, and how big each tab is.
const TAB_COLUMN_WIDTH: Pixels = px(44.);
const TAB_SIZE: Pixels = px(32.);
/// The agent's mark inside its tab.
const TAB_ICON_SIZE: Pixels = px(16.);
/// How tall the search box's row along the top of the panel is.
const SEARCH_ROW_HEIGHT: Pixels = px(40.);
/// The star's button on a model's row.
const STAR_BUTTON_SIZE: Pixels = px(20.);
/// The buttons below the prompt for the model, the effort and the mode: their height,
/// the icon before the name, and the caret after it.
const PICKER_HEIGHT: Pixels = px(28.);
const PICKER_ICON_SIZE: Pixels = px(14.);
const PICKER_CARET_SIZE: Pixels = px(12.);
/// The effort meter's tallest bar, and each bar's width and the gap between bars.
const METER_HEIGHT: Pixels = px(12.);
const METER_BAR_WIDTH: Pixels = px(2.);
/// Groups a model's row with its star, which shows while the row is pointed at.
const MODEL_ROW_GROUP: &str = "model-panel-row";

/// The colors of the model panel and the effort menu. In dark mode the surface is a step
/// lighter than the window (#1f1f1f over #151515): the theme's popover color, #0a0a0a,
/// is darker than the window and made the panel look sunk into it.
#[derive(Clone, Copy)]
struct MenuLook {
    dark: bool,
    surface: Hsla,
    /// Behind the panel's column of tabs.
    rail: Hsla,
    /// Between the panel's search box, its list and its switches.
    line: Hsla,
    /// Under the pointer.
    hover: Hsla,
    /// The tab on show, a button whose menu is open, the star's button being pressed.
    pressed: Hsla,
    foreground: Hsla,
    muted: Hsla,
    /// A starred model's star.
    star: Hsla,
    /// The effort meter's bars above the level.
    meter_off: Hsla,
}

impl MenuLook {
    fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let foreground = theme.foreground;
        if theme.is_dark() {
            Self {
                dark: true,
                surface: rgb(0x1f1f1f).into(),
                rail: rgb(0x1a1a1a).into(),
                line: white().opacity(0.08),
                hover: white().opacity(0.07),
                pressed: white().opacity(0.11),
                foreground,
                muted: theme.muted_foreground,
                star: rgb(0xF5B83D).into(),
                meter_off: white().opacity(0.18),
            }
        } else {
            Self {
                dark: false,
                surface: white(),
                rail: rgb(0xf6f6f6).into(),
                line: black().opacity(0.08),
                hover: black().opacity(0.05),
                pressed: black().opacity(0.08),
                foreground,
                // A shade darker than the theme's #737373, which is under 4.5:1 on the
                // tab column's gray.
                muted: rgb(0x6b6b6b).into(),
                // Darker than the dark mode's amber, to stay 3:1 on white.
                star: rgb(0xB07800).into(),
                meter_off: black().opacity(0.16),
            }
        }
    }

    /// A 1px ring around the surface and two soft shadows below it. GPUI's blur is the
    /// gaussian's deviation, half of CSS's blur radius, so 18 and 6 here are CSS's 36
    /// and 12.
    fn shadow(&self) -> Vec<BoxShadow> {
        let ink = |alpha| black().opacity(alpha);
        let (ring, far, near, far_spread, near_spread) = if self.dark {
            (0.6, 0.6, 0.4, -8., -2.)
        } else {
            (0.09, 0.2, 0.1, -10., -4.)
        };
        vec![
            BoxShadow::new(px(0.), px(0.), ink(ring)).spread_radius(px(1.)),
            BoxShadow::new(px(0.), px(16.), ink(far))
                .blur_radius(px(18.))
                .spread_radius(px(far_spread)),
            BoxShadow::new(px(0.), px(4.), ink(near))
                .blur_radius(px(6.))
                .spread_radius(px(near_spread)),
        ]
    }

    /// The surface of the panel or the effort menu, rounded and lifted off the window.
    fn surface(&self) -> Div {
        div()
            .relative()
            .bg(self.surface)
            .text_color(self.foreground)
            .text_size(MENU_TEXT_SIZE)
            .line_height(MENU_LINE_HEIGHT)
            .rounded(SURFACE_RADIUS)
            .shadow(self.shadow())
    }

    /// In dark mode, a hairline just inside the surface's edge, as macOS's menus have.
    /// It goes last, over the surface's content, so the tab column's gray can't hide it.
    fn edge(&self) -> Option<Div> {
        self.dark.then(|| {
            div()
                .absolute()
                .inset_0()
                .rounded(SURFACE_RADIUS)
                .border_1()
                .border_color(white().opacity(0.07))
        })
    }

    /// A row of the panel or the effort menu, which lights up under the pointer.
    fn row(&self, id: impl Into<ElementId>) -> Stateful<Div> {
        let hover = self.hover;
        div()
            .id(id)
            .h(LIST_ROW_HEIGHT)
            .flex_none()
            .pl(ROW_PAD_LEFT)
            .pr(ROW_PAD_RIGHT)
            .flex()
            .items_center()
            .gap(LEAD_GAP)
            .rounded(ROW_RADIUS)
            .hover(move |style| style.bg(hover))
    }

    /// The ghost look of the buttons below the prompt: a gray under the pointer and a
    /// stronger one while the button's menu is open.
    fn button(&self, cx: &App) -> ButtonCustomVariant {
        ButtonCustomVariant::new(cx)
            .foreground(self.foreground)
            .hover(self.hover)
            .active(self.pressed)
    }
}

/// The column a row starts with, for its check or mark.
fn lead() -> Div {
    div()
        .w(LEAD_WIDTH)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

/// A check in a row's `lead`, before the model or level in use.
fn check() -> Icon {
    Icon::new(IconName::Check).size(LEAD_ICON_SIZE)
}

/// A meter of how hard the model thinks: one bar for each of `levels` levels, rising to
/// the right, with those up to `level` in `on` and the rest in `off`.
fn effort_meter(level: usize, levels: usize, on: Hsla, off: Hsla) -> Div {
    div()
        .flex_none()
        .h(METER_HEIGHT)
        .flex()
        .items_end()
        .gap(METER_BAR_WIDTH)
        .children((0..levels).map(|bar| {
            // The last bar is full height and each one before it 2px shorter.
            let height = (METER_HEIGHT - px(2.) * (levels - 1 - bar) as f32).max(px(2.));
            div()
                .w(METER_BAR_WIDTH)
                .h(height)
                .rounded(px(1.))
                .bg(if bar <= level { on } else { off })
        }))
}

/// Where `choice` stands among the setting's levels, and how many there are, for the
/// effort meter. A "default" level has no place of its own, so it gets no meter, and
/// neither does a setting of fewer than two levels.
fn effort_level(setting: &Setting, choice: &SettingChoice) -> Option<(usize, usize)> {
    let levels: Vec<&SettingChoice> = listed_choices(setting)
        .filter(|level| level.id != "default")
        .collect();
    let at = levels.iter().position(|level| level.id == choice.id)?;
    (levels.len() >= 2).then_some((at, levels.len()))
}

/// A model's name as its row shows it: a trailing note in parentheses, such as
/// "(1M context)", in `muted`, and the part `query` matched in semibold.
fn model_name(name: &str, query: &str, muted: Hsla) -> StyledText {
    let note = name
        .rfind(" (")
        .filter(|_| name.ends_with(')'))
        .unwrap_or(name.len());
    // The list matches the lowercased name, whose letters can take another number of
    // bytes than the name's own. Then the match is left unmarked.
    let found = (!query.is_empty())
        .then(|| name.to_lowercase())
        .filter(|lower| lower.len() == name.len())
        .and_then(|lower| lower.find(query))
        .map(|start| start..start + query.len())
        .filter(|found| name.is_char_boundary(found.start) && name.is_char_boundary(found.end));
    let mut edges = vec![0, note, name.len()];
    if let Some(found) = &found {
        edges.extend([found.start, found.end]);
    }
    edges.sort_unstable();
    edges.dedup();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = edges
        .windows(2)
        .filter_map(|pair| {
            let range = pair[0]..pair[1];
            let in_note = range.start >= note;
            let matched = found
                .as_ref()
                .is_some_and(|found| range.start >= found.start && range.end <= found.end);
            (in_note || matched).then(|| {
                let style = HighlightStyle {
                    color: in_note.then_some(muted),
                    font_weight: matched.then_some(FontWeight::SEMIBOLD),
                    ..Default::default()
                };
                (range, style)
            })
        })
        .collect();
    StyledText::new(name.to_string()).with_highlights(highlights)
}

/// The harnesses' marks, for the panel's tabs.
const CLAUDE_LOGO: &[u8] = include_bytes!("../assets/claude.svg");
const OPENAI_LOGO: &[u8] = include_bytes!("../assets/openai.svg");
/// The orange Claude's mark is drawn in, in both light and dark mode.
const CLAUDE_ORANGE: u32 = 0xD97757;

/// The agent's mark for its tab: its harness's, or a terminal for a command.
fn agent_logo(name: &str) -> Icon {
    match harness_of(name) {
        Some(Harness::Claude) => Icon::empty()
            .data(CLAUDE_LOGO)
            .text_color(rgb(CLAUDE_ORANGE)),
        Some(Harness::Codex) => Icon::empty().data(OPENAI_LOGO),
        None => Icon::new(IconName::SquareTerminal),
    }
}

/// The kinds of setting whose "default" choice can stand for another: Claude's adapter
/// lists the model and the thought level that way. A mode named "default" is a mode.
const STAND_IN_KINDS: [SettingKind; 2] = [SettingKind::Model, SettingKind::ThoughtLevel];

/// The setting's "default" choice and the choice it stands for, when it stands for one:
/// its description names that choice. A "default" that names none is a choice of its
/// own, and is shown and listed like the others.
fn stand_in(setting: &Setting) -> Option<(&SettingChoice, &SettingChoice)> {
    if !STAND_IN_KINDS.contains(&setting.kind) {
        return None;
    }
    let default = setting
        .choices
        .iter()
        .find(|choice| choice.id == "default")?;
    let stands_for = default.description.as_deref()?;
    let choice = setting
        .choices
        .iter()
        .find(|choice| choice.id != "default" && choice.name == stands_for)?;
    Some((default, choice))
}

/// The setting's choices as they are listed: all of them but a "default" that stands
/// for one of the others.
fn listed_choices(setting: &Setting) -> impl Iterator<Item = &SettingChoice> {
    let hidden = stand_in(setting).map(|(default, _)| default.id.as_str());
    setting
        .choices
        .iter()
        .filter(move |choice| Some(choice.id.as_str()) != hidden)
}

/// The choice a setting shows as its value: the one it is set to, or the one its
/// "default" stands for when it is set to that.
fn shown_choice(setting: &Setting) -> Option<&SettingChoice> {
    let chosen = setting.chosen()?;
    match stand_in(setting) {
        Some((default, stands_for)) if default.id == chosen.id => Some(stands_for),
        _ => Some(chosen),
    }
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

/// A model the user starred: its agent's `kept_name` and its id. The agent goes by
/// name, not index, so the star holds across runs that start the agents in another
/// order, or start one of them by the `claude` or `codex` argument.
#[derive(Clone, PartialEq, Eq)]
struct Favorite {
    agent: String,
    model: String,
}

/// One agent's models in the model panel.
struct ModelGroup {
    agent: usize,
    name: String,
    /// The name the agent's favorites go by, from `kept_name`. None for a command,
    /// whose models can't be starred: the favorites file would name the command.
    kept: Option<&'static str>,
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
    look: MenuLook,
}

impl ModelPanel<'_> {
    /// A tab in the panel's left column, which shows `tab` when pressed.
    fn tab(&self, tab: ModelTab, icon: Icon, name: SharedString, selected: bool) -> Stateful<Div> {
        let view = self.view.clone();
        let look = self.look;
        let tooltip = name.clone();
        div()
            .id(ElementId::Name(format!("model-tab-{name}").into()))
            .size(TAB_SIZE)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(ROW_RADIUS)
            .cursor_pointer()
            .map(|tab| {
                if selected {
                    tab.bg(look.pressed).text_color(look.foreground)
                } else {
                    tab.text_color(look.muted)
                        .hover(move |style| style.bg(look.hover).text_color(look.foreground))
                }
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(icon.size(TAB_ICON_SIZE))
            .on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.show_model_tab(tab, cx));
            })
    }

    /// A model's row: a check before the session's model, the name, and its star. A
    /// press sets the session's model, or goes on to a session on the model's agent,
    /// and closes the panel. `query` is the search, which the name shows in bold.
    fn model_row(
        &self,
        group: &ModelGroup,
        choice: &SettingChoice,
        current: bool,
        starred: bool,
        query: &str,
    ) -> Stateful<Div> {
        let (view, popover, key) = (self.view.clone(), self.popover.clone(), self.key);
        let (agent, own) = (group.agent, group.own);
        let (setting, model) = (group.setting.id.clone(), choice.id.clone());
        self.look
            .row(ElementId::Name(
                format!("model-row-{agent}-{}", choice.id).into(),
            ))
            .group(MODEL_ROW_GROUP)
            .child(lead().when(current, |lead| lead.child(check())))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(model_name(&choice.name, query, self.look.muted)),
            )
            .children(
                group
                    .kept
                    .map(|kept| self.star(kept, group.agent, choice, starred)),
            )
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    if own {
                        let value = SettingValue::Choice(model.clone());
                        this.change_setting(key, setting.clone(), value, cx);
                    } else {
                        this.pick_model(key, agent, setting.clone(), model.clone(), window, cx);
                    }
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
    }

    /// The star at the end of a model's row, which stars the model or takes its star
    /// off. A starred model's star is filled in amber and always shows; the others show
    /// while their row is pointed at. `kept` is the agent's name in the favorites.
    fn star(
        &self,
        kept: &'static str,
        agent: usize,
        choice: &SettingChoice,
        starred: bool,
    ) -> Stateful<Div> {
        let view = self.view.clone();
        let look = self.look;
        let favorite = Favorite {
            agent: kept.to_string(),
            model: choice.id.clone(),
        };
        let label = if starred {
            "Remove from favorites"
        } else {
            "Add to favorites"
        };
        div()
            .id(ElementId::Name(
                format!("model-star-{agent}-{}", choice.id).into(),
            ))
            .role(Role::Button)
            .aria_label(label)
            .size(STAR_BUTTON_SIZE)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(ROW_RADIUS - px(1.))
            .map(|star| {
                if starred {
                    star.text_color(look.star)
                        .hover(move |style| style.bg(look.pressed))
                } else {
                    star.text_color(look.muted)
                        .hover(move |style| style.bg(look.pressed).text_color(look.foreground))
                        .opacity(0.)
                        .group_hover(MODEL_ROW_GROUP, |style| style.opacity(1.))
                }
            })
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
            .child(
                Icon::new(if starred {
                    IconName::StarFill
                } else {
                    IconName::Star
                })
                .size(LEAD_ICON_SIZE),
            )
            .on_click(move |_, _, cx| {
                // Doesn't pick the model, as a press elsewhere on its row does.
                cx.stop_propagation();
                let _ = view.update(cx, |this, cx| {
                    this.set_favorite(favorite.clone(), !starred, cx)
                });
            })
    }

    /// A line in the favorites naming the agent whose models follow it, with its mark
    /// where the models' checks go.
    fn group_header(&self, name: &str, first: bool) -> AnyElement {
        div()
            .h(LIST_HEADER_HEIGHT)
            .flex_none()
            .when(!first, |header| header.mt(LIST_HEADER_GAP))
            .pl(ROW_PAD_LEFT)
            .flex()
            .items_center()
            .gap(LEAD_GAP)
            .text_size(MENU_HEADING_SIZE)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(self.look.muted)
            .child(lead().child(agent_logo(name).size(HEADING_ICON_SIZE)))
            .child(SharedString::from(name.to_string()))
            .into_any_element()
    }

    /// A row along the panel's bottom for a setting of the model that is on or off.
    /// The switch only shows the value: a press anywhere on the row flips it.
    fn switch_row(&self, setting: &Setting, on: bool) -> Stateful<Div> {
        let (view, key, id) = (self.view.clone(), self.key, setting.id.clone());
        self.look
            .row(ElementId::Name(
                format!("model-switch-{}", setting.id).into(),
            ))
            .h(SWITCH_ROW_HEIGHT)
            .pr(ROW_PAD_LEFT)
            // Empty, so the name starts where the models' names do.
            .child(lead())
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(setting.name.clone())),
            )
            .child(
                Switch::new(ElementId::Name(
                    format!("model-switch-value-{}", setting.id).into(),
                ))
                .checked(on)
                .small()
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

/// `surface`, the model panel or the effort menu, coming in as it opens upward: it
/// rises out of the button below it as it fades in. The popover is drawn only while
/// open, so each open plays this afresh.
fn rise_in(
    surface: Div,
    id: impl Into<ElementId>,
    window: &Window,
    cx: &App,
) -> AnimationElement<Div> {
    let motion = cx.theme().motion_tokens();
    let travel = motion.distance_short.to_pixels(window.rem_size());
    let easing = motion.easing_enter.clone();
    let enter = Animation::new(motion.duration_normal).with_easing(move |t| easing.sample(t));
    come_in(surface, id, enter, move |surface, rest| {
        surface.top(travel * rest)
    })
}

/// The models of `group` that pass `only` and are named like `query`, with whether
/// each is the session's model. `only` goes first, as it is the cheaper test.
fn listed_models<'a>(
    group: &'a ModelGroup,
    only: impl Fn(&SettingChoice) -> bool,
    query: &str,
) -> Vec<(&'a SettingChoice, bool)> {
    let current = group
        .own
        .then(|| shown_choice(&group.setting))
        .flatten()
        .map(|choice| choice.id.as_str());
    listed_choices(&group.setting)
        .filter(|choice| only(choice))
        .filter(|choice| query.is_empty() || choice.name.to_lowercase().contains(query))
        .map(|choice| (choice, current == Some(choice.id.as_str())))
        .collect()
}

/// What the panel shows in place of its list when the tab has no models to show: an
/// icon, what is missing, and for the empty favorites, how to fill them.
fn empty_list(icon: Icon, title: String, hint: Option<&str>, look: MenuLook) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .p(px(24.))
        .whitespace_normal()
        .text_center()
        .child(icon.size(px(18.)).text_color(look.muted).mb(px(2.)))
        .child(title)
        .children(hint.map(|hint| {
            div()
                .text_xs()
                .line_height(px(16.))
                .text_color(look.muted)
                .child(SharedString::from(hint.to_string()))
        }))
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
    let look = panel.look;
    let typed = panel.search.read(cx).value().trim().to_string();
    let query = typed.to_lowercase();
    let is_starred = |group: &ModelGroup, choice: &SettingChoice| {
        group.kept.is_some_and(|kept| {
            favorites
                .iter()
                .any(|favorite| favorite.agent == kept && favorite.model == choice.id)
        })
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    let empty = match tab {
        ModelTab::Favorites => {
            // Each agent's starred models under its name.
            for group in groups {
                let starred = listed_models(group, |choice| is_starred(group, choice), &query);
                if starred.is_empty() {
                    continue;
                }
                rows.push(panel.group_header(&group.name, rows.is_empty()));
                rows.extend(starred.into_iter().map(|(choice, current)| {
                    panel
                        .model_row(group, choice, current, true, &query)
                        .into_any_element()
                }));
            }
            if query.is_empty() {
                (
                    Icon::new(IconName::Star),
                    "No favorites yet".to_string(),
                    Some("Star a model to keep it here."),
                )
            } else {
                (
                    Icon::new(gpui_kit::assets::IconName::SearchX),
                    format!("No favorites match \u{201c}{typed}\u{201d}"),
                    None,
                )
            }
        }
        ModelTab::Agent(agent) => {
            for group in groups.iter().filter(|group| group.agent == agent) {
                rows.extend(listed_models(group, |_| true, &query).into_iter().map(
                    |(choice, current)| {
                        let starred = is_starred(group, choice);
                        panel
                            .model_row(group, choice, current, starred, &query)
                            .into_any_element()
                    },
                ));
            }
            let title = if query.is_empty() {
                "No models".to_string()
            } else {
                format!("No models match \u{201c}{typed}\u{201d}")
            };
            (Icon::new(gpui_kit::assets::IconName::SearchX), title, None)
        }
    };

    // Set apart from the list by its gray alone, with no line between them.
    let tabs = div()
        .w(TAB_COLUMN_WIDTH)
        .flex_none()
        .py(px(6.))
        .flex()
        .flex_col()
        .items_center()
        .gap(SURFACE_PADDING)
        .bg(look.rail)
        // GPUI clips the panel's content to its box, not its rounded corners.
        .rounded_l(SURFACE_RADIUS)
        .child(panel.tab(
            ModelTab::Favorites,
            Icon::new(IconName::Star),
            "Favorites".into(),
            tab == ModelTab::Favorites,
        ))
        .children(groups.iter().map(|group| {
            panel.tab(
                ModelTab::Agent(group.agent),
                agent_logo(&group.name),
                group.name.clone().into(),
                tab == ModelTab::Agent(group.agent),
            )
        }));
    // The magnifier sits in the rows' lead column and the text where their names start.
    let search = div()
        .h(SEARCH_ROW_HEIGHT)
        .flex_none()
        .pl(SURFACE_PADDING + ROW_PAD_LEFT)
        .pr(px(12.))
        .flex()
        .items_center()
        .gap(LEAD_GAP)
        .border_b_1()
        .border_color(look.line)
        .child(
            lead()
                .text_color(look.muted)
                .child(Icon::new(IconName::Search).size(LEAD_ICON_SIZE)),
        )
        .child(
            Input::new(panel.search)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .small()
                .px_0()
                .text_size(MENU_TEXT_SIZE)
                .flex_1(),
        );
    let list = if rows.is_empty() {
        let (icon, title, hint) = empty;
        empty_list(icon, title, hint, look).into_any_element()
    } else {
        div()
            .id("model-panel-rows")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p(SURFACE_PADDING)
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    };
    let footer = (!switches.is_empty()).then(|| {
        div()
            .flex_none()
            .p(SURFACE_PADDING)
            .border_t_1()
            .border_color(look.line)
            .flex()
            .flex_col()
            .children(switches.iter().map(|setting| {
                let on = matches!(setting.value, SettingValue::Switch(true));
                panel.switch_row(setting, on)
            }))
    });

    let surface = look
        .surface()
        .w(PANEL_WIDTH)
        .h(PANEL_HEIGHT)
        .max_h(window.viewport_size().height * 0.8)
        .overflow_hidden()
        .flex()
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
        )
        .children(look.edge());
    rise_in(surface, "model-panel-in", window, cx)
}

/// The effort menu: the setting's name over its levels, each with a check before the
/// level in use and the meter after it. A press sets the level and closes the menu.
fn effort_menu(
    setting: &Setting,
    view: &WeakEntity<ChatView>,
    key: SessionKey,
    popover: &Entity<PopoverState>,
    window: &Window,
    cx: &App,
) -> AnimationElement<Div> {
    let look = MenuLook::new(cx);
    let current = shown_choice(setting).map(|choice| choice.id.as_str());
    let heading = div()
        .h(MENU_HEADING_HEIGHT)
        .flex_none()
        // Level with the levels' names.
        .pl(ROW_PAD_LEFT + LEAD_WIDTH + LEAD_GAP)
        .flex()
        .items_center()
        .text_size(MENU_HEADING_SIZE)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(look.muted)
        .child(SharedString::from(setting.name.clone()));
    let rows = listed_choices(setting).map(|choice| {
        let (view, popover) = (view.clone(), popover.clone());
        let (id, value) = (setting.id.clone(), choice.id.clone());
        look.row(ElementId::Name(format!("effort-{}", choice.id).into()))
            .child(lead().when(current == Some(choice.id.as_str()), |lead| {
                lead.child(check())
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(choice.name.clone())),
            )
            .children(effort_level(setting, choice).map(|(level, levels)| {
                effort_meter(level, levels, look.muted, look.meter_off).mr(px(4.))
            }))
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    let value = SettingValue::Choice(value.clone());
                    this.change_setting(key, id.clone(), value, cx)
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
    });
    let surface = look
        .surface()
        .w(EFFORT_MENU_WIDTH)
        .p(SURFACE_PADDING)
        .flex()
        .flex_col()
        .whitespace_nowrap()
        .child(heading)
        .children(rows)
        .children(look.edge());
    rise_in(surface, "effort-menu-in", window, cx)
}

/// How wide a mode's name and description may run, each on one line. Codex's longest
/// description is about 500px; anything past this is cut short with an ellipsis.
const MODE_TEXT_MAX_WIDTH: Pixels = px(560.);

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
                    // The cap has to be here, not on the menu: asked for its narrowest
                    // width, gpui's text answers with its whole line, so a row under a
                    // narrower menu keeps that width and runs past the menu's edge.
                    .max_w(MODE_TEXT_MAX_WIDTH)
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(div().line_height(px(20.)).child(name.clone()))
                    .children(
                        description.clone().map(|description| {
                            div().text_xs().text_color(muted).child(description)
                        }),
                    ),
            )
            // Kept when empty, so every row leaves its text the same room.
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

/// A button below the prompt: an icon, a name and a caret, in 13px text on a 28px
/// row, a step smaller than the component library's own, to match the menus they open.
/// It grays under the pointer and stays gray while its menu is open.
fn picker_button(
    id: &'static str,
    icon: Option<AnyElement>,
    name: String,
    accessibility_label: String,
    cx: &App,
) -> InputGroupButton {
    let look = MenuLook::new(cx);
    InputGroupButton::new(id)
        .custom(look.button(cx))
        .small()
        .accessibility_label(accessibility_label)
        .h(PICKER_HEIGHT)
        .pl(px(10.))
        .pr(px(8.))
        .rounded(ROW_RADIUS)
        .children(icon)
        .child(
            // The button's own 14px text is set on the row the name sits in.
            div()
                .min_w(px(0.))
                .text_size(MENU_TEXT_SIZE)
                .line_height(MENU_LINE_HEIGHT)
                .text_ellipsis()
                .child(SharedString::from(name)),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .size(PICKER_CARET_SIZE)
                .text_color(look.muted),
        )
}

/// The model's button, named for the model, with the mark of the session's agent. An
/// agent that offers no model still gets the button, for the other agents' models and
/// its switches.
fn model_button(settings: &[Setting], agent: &str, cx: &App) -> InputGroupButton {
    let name = settings
        .iter()
        .find(|setting| setting.kind == SettingKind::Model)
        .and_then(shown_choice)
        .map_or_else(|| "Model".to_string(), |choice| choice.name.clone());
    let icon = agent_logo(agent).size(PICKER_ICON_SIZE).into_any_element();
    let label = format!("Model: {name}");
    picker_button("model-picker", Some(icon), name, label, cx)
}

/// The button for how hard the model thinks, named for the level, with its meter.
fn effort_button(setting: &Setting, cx: &App) -> InputGroupButton {
    let look = MenuLook::new(cx);
    let shown = shown_choice(setting);
    let name = shown.map_or_else(|| setting.name.clone(), |choice| choice.name.clone());
    let meter = shown
        .and_then(|choice| effort_level(setting, choice))
        .map(|(level, levels)| {
            effort_meter(level, levels, look.foreground, look.meter_off).into_any_element()
        });
    let label = format!("{}: {name}", setting.name);
    picker_button("effort-picker", meter, name, label, cx)
}

/// The mode's button, with the mode's icon.
fn mode_button(setting: &Setting, cx: &App) -> InputGroupButton {
    let current = setting.chosen();
    let name = current.map_or_else(|| setting.name.clone(), |choice| choice.name.clone());
    let icon = Icon::new(mode_icon(current.map_or("", |choice| choice.id.as_str())))
        .size(PICKER_ICON_SIZE)
        .into_any_element();
    let label = format!("{}: {name}", setting.name);
    picker_button("mode-picker", Some(icon), name, label, cx)
}

impl ChatView {
    /// The model's button, named for the session's model, and the panel of models that
    /// opens upward from it: the favorites, the session's own agent's models, and the
    /// other agents'. The session's settings come as `shown`.
    fn render_model_picker(
        &self,
        session: &Session,
        shown: &[Setting],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Each agent's models, in the agents' order: the session's own agent's as
        // `shown` has them, the others' as a session on each has them, else as the
        // agent last created a session with them. An agent that is gone can't take a
        // session, so its models aren't offered.
        let groups: Vec<ModelGroup> = self
            .agents
            .iter()
            .enumerate()
            .filter_map(|(ix, slot)| {
                let own = ix == session.agent;
                if !own && slot.commands.is_closed() {
                    return None;
                }
                let settings: &[Setting] = if own {
                    shown
                } else {
                    self.model_source(ix)
                        .map_or(&slot.last_settings, |source| &source.settings)
                };
                let setting = settings
                    .iter()
                    .find(|setting| setting.kind == SettingKind::Model)?;
                Some(ModelGroup {
                    agent: ix,
                    name: slot.name.clone(),
                    kept: kept_name(&slot.name),
                    setting: setting.clone(),
                    own,
                })
            })
            .collect();
        let switches: Vec<Setting> = shown
            .iter()
            .filter(|setting| setting.kind == SettingKind::ModelConfig)
            .cloned()
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
        // The tab on show may be an agent's that went away while the panel was open.
        let tab = match self.model_panel.tab {
            ModelTab::Agent(agent) if !groups.iter().any(|group| group.agent == agent) => first_tab,
            tab => tab,
        };
        let search = self.model_panel.search.clone();
        let favorites = self.favorites.clone();
        let trigger = model_button(shown, &self.agents[session.agent].name, cx).cursor_pointer();
        let picker = Popover::new("model-panel")
            .appearance(false)
            .anchor(Anchor::BottomLeft)
            .offset(MENU_OFFSET)
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
                let panel = ModelPanel {
                    view: &view,
                    key,
                    popover: &popover,
                    search: &search,
                    look: MenuLook::new(cx),
                };
                model_panel(&panel, &groups, &switches, tab, &favorites, window, cx)
            });
        Some(picker.into_any_element())
    }

    /// The button for how hard the model thinks, and a menu of the levels that opens
    /// upward. A "default" level that stands for one of the others is left out. The menu
    /// is a popover of its own rather than the component library's menu, whose surface
    /// and rows can't take the model panel's look.
    fn render_effort_picker(
        &self,
        key: SessionKey,
        setting: &Setting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let setting = setting.clone();
        Popover::new("effort-menu")
            .appearance(false)
            .anchor(Anchor::BottomLeft)
            .offset(MENU_OFFSET)
            .trigger(effort_button(&setting, cx).cursor_pointer())
            .content(move |_, window, cx| {
                let popover = cx.entity();
                effort_menu(&setting, &view, key, &popover, window, cx)
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
        mode_button(&setting, cx)
            .cursor_pointer()
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, _| {
                // As wide as its widest row; `mode_item` caps how wide a row gets.
                // Left unset, gpui-component's own cap is 500px, short of a long row.
                let menu = menu.min_w(px(260.)).max_w(window.viewport_size().width);
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
        // The model, how hard it thinks, then the mode, each once the agent has said
        // what it can be, for this session or the last it created.
        let shown = self.shown_settings(session);
        let pickers = [
            self.render_model_picker(session, &shown, cx),
            shown
                .iter()
                .find(|setting| setting.kind == SettingKind::ThoughtLevel)
                .map(|setting| self.render_effort_picker(session.key, setting, cx)),
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
        // 6px past the zoom button leaves about 12px before the sidebar button's outline.
        // Without the traffic lights, the outline lines up with the sidebar's rows.
        let toggle_left = if title_bar {
            self.traffic_lights_end + px(6.)
        } else {
            px(14.25)
        };
        // A session has no name until its first prompt, and the row stays empty till then.
        let name = session.title.clone();
        // Level with the transcript's text, until the sidebar sliding away brings the
        // name up to the sidebar button; from there it stays 6px past the button.
        let name_left = (toggle_left + SIDEBAR_BUTTON_SIZE + px(6.) - sidebar_width).max(px(16.));

        // The chat fills the window right of the sidebar.
        let chat = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(background)
            .child(
                // The session's name, in the window's top row. While the sidebar is
                // hidden the traffic lights and the sidebar button sit here too.
                div()
                    .flex_none()
                    .h(self.title_bar_height)
                    .pl(name_left)
                    .pr_4()
                    .flex()
                    .items_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .children(name.map(|name| div().flex_1().min_w(px(0.)).truncate().child(name)))
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
                                // model button's mark, and its capitals 17px down.
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
            .top((self.title_bar_height - SIDEBAR_BUTTON_SIZE) / 2.)
            .left(toggle_left)
            .size(SIDEBAR_BUTTON_SIZE)
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

/// Holds the models the user starred, one a line: the agent's `kept_name`, a tab, the
/// model's id.
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

/// The harness an agent is, whether the window started it as that or by the `claude`
/// or `codex` argument. None for any other command.
fn harness_of(name: &str) -> Option<Harness> {
    Harness::ALL
        .into_iter()
        .find(|harness| harness.name().eq_ignore_ascii_case(name))
}

/// The name an agent's settings and favorites are kept under on disk: its harness's.
/// None for any other command, which is all that names such an agent and may carry a
/// secret, such as a key, so nothing of it is written.
fn kept_name(name: &str) -> Option<&'static str> {
    harness_of(name).map(Harness::name)
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

#[cfg(feature = "profile")]
actions!(acp_gui, [CycleFrameOverlay]);

/// With `profile`, sends the agent's answer times and the protocol's warnings to
/// stderr, and every info span and event to Tracy while it is connected.
#[cfg(feature = "profile")]
fn start_tracing() {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::prelude::*;

    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(
            Targets::new()
                .with_target("acp_core", LevelFilter::INFO)
                .with_target("agent_client_protocol", LevelFilter::WARN),
        );
    tracing_subscriber::registry()
        .with(stderr)
        // Also starts Tracy's client, which gpui's zones need before the first frame.
        .with(tracing_tracy::TracyLayer::default().with_filter(LevelFilter::INFO))
        .init();
}

/// With `profile`, Cmd+Option+F cycles gpui's frame-time overlay: hidden, the last
/// frame's draw time, then that with the slowest 1% and 10%, the worst, and a count.
#[cfg(feature = "profile")]
fn bind_frame_overlay(cx: &mut App) {
    cx.on_action(|_: &CycleFrameOverlay, cx| {
        let Some(window) = cx.active_window() else {
            return;
        };
        // A key press is dispatched from inside the window's own update, where the
        // window can't be updated again.
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| window.cycle_debug_frame_overlay_mode());
        });
    });
    cx.bind_keys([KeyBinding::new("cmd-alt-f", CycleFrameOverlay, None)]);
}

fn main() {
    #[cfg(feature = "profile")]
    start_tracing();
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
            #[cfg(feature = "profile")]
            bind_frame_overlay(cx);
            cx.set_menus([Menu::new("ACP client").items([
                #[cfg(feature = "profile")]
                MenuItem::action("Cycle frame-time overlay", CycleFrameOverlay),
                MenuItem::action("Quit ACP client", Quit),
            ])]);
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
