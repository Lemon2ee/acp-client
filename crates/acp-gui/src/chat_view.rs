//! The window's one view: the agents it drives, its sessions and what happens to them, and
//! how the sidebar, the chat and the settings are laid out. The parts it draws live in the
//! modules below it, which can read its private fields.

mod pickers;
pub(crate) mod scroller;
mod settings_page;
mod sidebar;
mod tool_call;
mod transcript;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use acp_core::{
    AcpAgent, Command, Event as AgentEvent, Sender, SessionEvent, SessionKey, Setting, SettingKind,
    SettingValue,
};
use gpui_kit::base::input::Copy;
use gpui_kit::base::{TextSelection, Transition, transition};
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{
    InputEvent, InputGroup, InputGroupAddon, InputGroupAddonAlignment, InputGroupButton, Textarea,
    TextareaState,
};
use gpui_kit::component::{ActiveTheme as _, FocusableExt as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::OpenSettings;
use crate::chat_view::pickers::MenuNav;
use crate::chat_view::pickers::effort::EffortMeter;
use crate::chat_view::pickers::model_panel::ModelPanelState;
use crate::chat_view::settings_page::SettingsPageState;
use crate::chat_view::sidebar::{ResizeSidebar, SidebarState};
use crate::chrome::{
    BLURRED_CHAT_ALPHA, BLURRED_SIDEBAR_ALPHA, OWN_TITLE_BAR, WINDOW_BLUR, apply_appearance,
    as_title_bar, window_background,
};
use crate::config::{
    AppSettings, SavedSession, change_saved_sessions, config_folder, kept_name, load_last_settings,
    load_saved_sessions, save_last_settings,
};
use crate::hotkey::{self, GlobalShortcut};
use crate::session::{Held, Origin, Row, Session};

/// How often text held back in a live turn shows its next piece: about once a frame.
const PACE_TICK: Duration = Duration::from_millis(16);
/// How long a held setting waits after its last change before it goes to the agent (see
/// `ChatView::hold_setting`).
const HOLD_PAUSE: Duration = Duration::from_millis(450);

/// One agent the window drives, and what it keeps ready on it.
struct AgentSlot {
    /// What the agent is called in the window: its harness's name, or the command it
    /// was started with.
    name: String,
    /// The command line that started it, for the settings' list of agents.
    command: String,
    /// The agent has said it is ready, so sessions can be created on it.
    connected: bool,
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

pub(crate) struct ChatView {
    /// Never empty: the window starts with the sessions kept from earlier runs, oldest
    /// first, and one new session, which its first prompt has the agent create. At most
    /// one has no prompt yet; the sidebar leaves it out, and a new one on another agent
    /// takes its place.
    sessions: Vec<Session>,
    /// Index into `sessions` of the one on screen.
    active: usize,
    next_key: u64,
    input: Entity<TextareaState>,
    /// Never empty. New sessions go on the agent of the session on screen, until the
    /// user picks another agent's model.
    agents: Vec<AgentSlot>,
    /// The folder the agents create new sessions in.
    cwd: PathBuf,
    /// The window's top row, across the sidebar and the chat. It holds the traffic lights
    /// on macOS, the sidebar button, and the name of the session on screen.
    title_bar_height: Pixels,
    /// Where the traffic lights end, from the window's left edge.
    traffic_lights_end: Pixels,
    /// The blur is on: the desktop shows through the window, blurred, behind the sidebar
    /// and the chat, except in full screen.
    window_blur: bool,
    /// The window is blurred now, and its sidebar and chat translucent.
    blurred: bool,
    sidebar: SidebarState,
    model_panel: ModelPanelState,
    /// The light and the focus of the menu below the prompt that is open.
    menu_nav: MenuNav,
    effort: EffortMeter,
    settings_page: SettingsPageState,
    /// The system-wide shortcut the settings name, as macOS holds it for the app.
    global_shortcut: GlobalShortcut,
    /// Shows the text held back in live turns a piece at a time, while any is held.
    pacing: Option<Task<()>>,
    /// Draws the window again each second while a tool call on screen counts its seconds.
    tool_clock: Option<Task<()>>,
    _input_events: Subscription,
    _bounds_events: Subscription,
    _settings_events: Subscription,
    _activation_events: Subscription,
    _recorder_keys: Subscription,
    _card_keys: Subscription,
    _layout_events: Subscription,
    /// None where there is no system-wide shortcut.
    _shortcut_presses: Option<Task<()>>,
}

impl ChatView {
    /// Starts every agent in `agents`, each by name, lists the sessions kept from
    /// earlier runs on them, and shows a session on the first that no agent creates
    /// until its first prompt. `blur` is whether the window opened blurred (see
    /// `window_background`).
    pub(crate) fn new(
        agents: Vec<(String, AcpAgent)>,
        cwd: PathBuf,
        blur: bool,
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
        // Going into full screen or out of it resizes the window.
        let bounds_events = cx.observe_window_bounds(window, |this, window, cx| {
            this.update_window_background(window, cx)
        });
        window.on_window_should_close(cx, |_, cx| {
            cx.quit();
            true
        });
        let settings_events = cx.observe_global_in::<AppSettings>(window, |this, window, cx| {
            this.apply_settings(window, cx)
        });
        // The recorder lets go of the shortcut while it records, so it stops when the
        // window goes to the back rather than leave the shortcut let go of.
        let activation_events = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.stop_recording(cx);
            }
        });
        // Before the window's own shortcuts, so that a key being recorded does nothing
        // else, even Cmd+Q.
        let view = cx.entity().downgrade();
        let recorder_keys = cx.intercept_keystrokes(move |event, _, cx| {
            let taken = view
                .update(cx, |this, cx| this.record_keystroke(&event.keystroke, cx))
                .unwrap_or_default();
            if taken {
                cx.stop_propagation();
            }
        });
        // Before the prompt's own Enter and the window's Esc, which a permission card
        // answers to while the prompt is empty (see `card_with_keys`).
        let view = cx.entity().downgrade();
        let card_keys = cx.intercept_keystrokes(move |event, window, cx| {
            let taken = view
                .update(cx, |this, cx| {
                    this.card_keystroke(&event.keystroke, window, cx)
                })
                .unwrap_or_default();
            if taken {
                cx.stop_propagation();
            }
        });
        // macOS holds the shortcut by the key that types it, which another layout may
        // put elsewhere.
        let view = cx.entity().downgrade();
        let layout_events = cx.on_keyboard_layout_change(move |cx| {
            let _ = view.update(cx, |this, cx| {
                this.global_shortcut.sync(true, cx);
                cx.notify();
            });
        });
        // Listened for before the shortcut is first held, so that no press is missed.
        let shortcut_presses = hotkey::presses().map(|presses| {
            cx.spawn_in(window, async move |this, cx| {
                while presses.recv().await.is_ok() {
                    let pressed = this.update_in(cx, |_, window, cx| {
                        let hide = cx.global::<AppSettings>().hide_when_in_front;
                        hotkey::bring_to_front(hide, window, cx);
                    });
                    if pressed.is_err() {
                        break;
                    }
                }
            })
        });
        // The app menu's Settings… and Cmd+, come here from wherever the focus is, or from
        // nowhere, as nothing in the window handles them. That is inside the window's own
        // update, where the window can't be reached, so the settings open just after.
        let (handle, view) = (window.window_handle(), cx.entity().downgrade());
        // `App`'s own `on_action`, as `Context` has one for a window's.
        App::on_action(cx, move |_: &OpenSettings, cx| {
            let view = view.clone();
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    view.update(cx, |this, cx| this.open_settings(window, cx))
                });
            });
        });

        let mut last_settings = load_last_settings();
        let agents = agents
            .into_iter()
            .enumerate()
            .map(|(ix, (name, agent))| {
                let config = agent.config();
                let command = std::iter::once(config.command().display().to_string())
                    .chain(config.arguments().iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ");
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
                    command,
                    connected: false,
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
            cwd,
            title_bar_height,
            traffic_lights_end,
            window_blur: blur,
            blurred: blur,
            sidebar: SidebarState::default(),
            model_panel: ModelPanelState::new(window, cx),
            menu_nav: MenuNav::new(cx),
            effort: EffortMeter::new(cx),
            settings_page: SettingsPageState::new(cx),
            global_shortcut: GlobalShortcut::default(),
            pacing: None,
            tool_clock: None,
            _input_events: input_events,
            _bounds_events: bounds_events,
            _settings_events: settings_events,
            _activation_events: activation_events,
            _recorder_keys: recorder_keys,
            _card_keys: card_keys,
            _layout_events: layout_events,
            _shortcut_presses: shortcut_presses,
        };
        view.global_shortcut.sync(false, cx);
        view.restore_sessions(cx);
        // On the first agent, whatever agent the oldest kept session is on, and even if
        // that agent is already gone, so there is always a session to show.
        let ix = view.untitled_on(0, cx);
        view.show(ix, window, cx);
        view
    }

    /// Lists the sessions kept from earlier runs on the window's agents. None is loaded
    /// until it shows, as Claude's adapter starts a Claude Code process for each.
    fn restore_sessions(&mut self, cx: &mut App) {
        for saved in load_saved_sessions() {
            let agent = self
                .agents
                .iter()
                .position(|slot| kept_name(&slot.name) == Some(saved.agent.as_str()));
            let Some(agent) = agent else {
                continue;
            };
            let key = SessionKey(self.next_key);
            self.next_key += 1;
            let mut session = Session::new(key, agent, cx);
            session.title = Some(saved.title);
            session.title_source = saved.title_source;
            session.id = Some(saved.id);
            session.origin = Origin::Saved(saved.cwd);
            session.archived = saved.archived;
            self.sessions.push(session);
        }
    }

    /// Has the agent load a session kept from an earlier run, as it first shows. Its
    /// transcript comes in as the agent replays it, and it takes prompts once loaded.
    fn load(&mut self, ix: usize, cx: &mut App) {
        let session = &mut self.sessions[ix];
        let (Origin::Saved(cwd), Some(id)) = (&session.origin, &session.id) else {
            return;
        };
        let load = Command::LoadSession(session.key, id.clone(), cwd.clone());
        session.origin = Origin::Loaded(cwd.clone());
        session.busy = true;
        let slot = &self.agents[session.agent];
        if slot.commands.try_send(load).is_err() {
            let message = format!("{} has gone; restart to use it", slot.name);
            session.push_error(message, cx);
        }
    }

    /// Keeps the session for later runs once it has both the agent's id and a title.
    /// Only Claude's and Codex's are kept: any other agent is named only by its
    /// command, which may carry a secret. One from an earlier run is kept already.
    fn save_session(&self, key: SessionKey) {
        let Some(session) = self.session(key) else {
            return;
        };
        let (Some(id), Some(title)) = (&session.id, &session.title) else {
            return;
        };
        let Some(agent) = kept_name(&self.agents[session.agent].name) else {
            return;
        };
        if !matches!(session.origin, Origin::New) {
            return;
        }
        let saved = SavedSession {
            agent: agent.to_string(),
            id: id.clone(),
            title: title.clone(),
            cwd: self.cwd.clone(),
            archived: session.archived,
            title_source: session.title_source,
        };
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| change_saved_sessions(&folder, |sessions| sessions.push(saved)));
        if let Err(err) = changed {
            eprintln!("couldn't keep the session for later runs: {err}");
        }
    }

    /// Takes up a change to the app's settings. The theme is the app's, so a new
    /// appearance repaints every window.
    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        apply_appearance(window, cx);
        let blur = cx.global::<AppSettings>().window_blur;
        self.set_window_blur(blur, window, cx);
        self.global_shortcut.sync(false, cx);
        cx.notify();
    }

    /// Has the desktop show through the window, blurred, behind the sidebar and the chat,
    /// or stop. Where gpui can't blur (see `WINDOW_BLUR`) the window stays opaque.
    fn set_window_blur(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.window_blur = on && WINDOW_BLUR;
        self.update_window_background(window, cx);
    }

    /// Blurs the window while the blur is on and the window isn't in full screen, and
    /// makes it opaque otherwise. In full screen the window fills a space of its own,
    /// and being opaque there spares the window server blending it with what's behind.
    fn update_window_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let blurred = self.window_blur && !window.is_fullscreen();
        // Called on every resize, so only a change reaches the platform.
        if blurred == self.blurred {
            return;
        }
        self.blurred = blurred;
        window.set_background_appearance(window_background(blurred));
        cx.notify();
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
            session.push_error(message, cx);
        }
        session
    }

    /// Makes a spare session for each agent whose models the model menu could list no
    /// other way: it has no spare, no session on show that it will create or load, and
    /// no settings kept from before. None is made before the window's first prompt or
    /// load, so starting the window has no agent create a session.
    fn make_spares(&mut self, cx: &mut App) {
        let asked = |session: &Session| {
            session.title.is_some() && !matches!(session.origin, Origin::Saved(_))
        };
        if !self.sessions.iter().any(asked) {
            return;
        }
        for agent in 0..self.agents.len() {
            let slot = &self.agents[agent];
            if slot.spare.is_some()
                || !slot.last_settings.is_empty()
                || slot.commands.is_closed()
                || self.sessions.iter().any(|session| {
                    session.agent == agent && !matches!(session.origin, Origin::Saved(_))
                })
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

    /// Shows a session with no prompt yet on the agent of the one on screen.
    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.sessions[self.active].agent;
        if self.agents[agent].commands.is_closed() {
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

    /// Puts the session at `ix` on screen; unsent text stays with the session it was typed
    /// in. One kept from an earlier run loads now.
    fn show(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.load(ix, cx);
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

    /// Keeps the session's title as it now stands, and where it came from, for later runs.
    fn keep_title(&self, key: SessionKey) {
        let Some(session) = self.session(key) else {
            return;
        };
        let Some(title) = session.title.clone() else {
            return;
        };
        let source = session.title_source;
        self.change_kept(key, |saved| {
            saved.title = title;
            saved.title_source = source;
        });
    }

    /// Makes `change` to the session as kept for later runs, if it is kept already.
    /// One whose agent hasn't given it an id yet is kept as it stands once the agent does.
    fn change_kept(&self, key: SessionKey, change: impl FnOnce(&mut SavedSession)) {
        let Some(session) = self.session(key) else {
            return;
        };
        let (Some(id), Some(agent)) = (&session.id, kept_name(&self.agents[session.agent].name))
        else {
            return;
        };
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| {
                change_saved_sessions(&folder, |sessions| {
                    let kept = sessions
                        .iter_mut()
                        .find(|saved| saved.agent == agent && saved.id == *id);
                    if let Some(saved) = kept {
                        change(saved);
                    }
                })
            });
        if let Err(err) = changed {
            eprintln!("couldn't change the session kept for later runs: {err}");
        }
    }

    /// Moves the session to the sidebar's Archived list, or back out of it. On screen,
    /// an archived session gives way as a deleted one does.
    fn set_archived(
        &mut self,
        key: SessionKey,
        archived: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.sessions.iter().position(|session| session.key == key) else {
            return;
        };
        if self.sessions[ix].archived == archived {
            return;
        }
        self.sessions[ix].archived = archived;
        self.change_kept(key, |saved| saved.archived = archived);
        if archived {
            if ix == self.active {
                let next = self.untitled_on(self.sessions[ix].agent, cx);
                self.show(next, window, cx);
            }
            self.put_away(ix, cx);
            // Its agent may have no session on show now.
            self.make_spares(cx);
        }
        cx.notify();
    }

    /// Has the agent close the session at `ix`, which is kept for later runs, so it holds
    /// nothing for it: Claude's adapter keeps a Claude Code process for each session. The
    /// session is then as it was after a restart, and loads again as it next shows. One
    /// not kept, or with a turn running, stays open: closing it would lose it, or stop the
    /// turn.
    fn put_away(&mut self, ix: usize, cx: &mut App) {
        let session = &self.sessions[ix];
        let folder = match &session.origin {
            Origin::New => self.cwd.clone(),
            Origin::Saved(_) => return,
            Origin::Loaded(folder) => folder.clone(),
        };
        let kept = kept_name(&self.agents[session.agent].name).is_some();
        if session.busy || session.id.is_none() || !kept {
            return;
        }
        // Under a new key, so nothing the agent still sends for the old one lands on it.
        let mut fresh = Session::new(SessionKey(self.next_key), session.agent, cx);
        self.next_key += 1;
        fresh.title = session.title.clone();
        fresh.title_source = session.title_source;
        fresh.id = session.id.clone();
        fresh.origin = Origin::Saved(folder);
        fresh.archived = session.archived;
        let old = std::mem::replace(&mut self.sessions[ix], fresh);
        self.sessions[ix].draft = old.draft;
        let _ = self.agents[old.agent]
            .commands
            .try_send(Command::CloseSession(old.key));
    }

    /// Takes the session out of the window and out of those kept for later runs, and
    /// has its agent close and delete it. On screen, the session with no prompt yet on
    /// its agent takes its place.
    fn delete_session(&mut self, key: SessionKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.sessions.iter().position(|session| session.key == key) else {
            return;
        };
        if self
            .sidebar
            .renaming
            .as_ref()
            .is_some_and(|renaming| renaming.key == key)
        {
            self.finish_rename(false, cx);
        }
        if ix == self.active {
            let next = self.untitled_on(self.sessions[ix].agent, cx);
            self.show(next, window, cx);
        }
        let session = self.sessions.remove(ix);
        if ix < self.active {
            self.active -= 1;
        }
        let slot = &self.agents[session.agent];
        let command = match session.id.clone() {
            Some(id) => Command::DeleteSession(session.key, id),
            // The agent is still creating it, and has nothing of it to delete: its first
            // prompt never runs.
            None => Command::CloseSession(session.key),
        };
        let _ = slot.commands.try_send(command);
        if let (Some(id), Some(agent)) = (session.id, kept_name(&slot.name)) {
            let changed = config_folder()
                .ok_or_else(|| std::io::Error::other("there is no config folder"))
                .and_then(|folder| {
                    change_saved_sessions(&folder, |sessions| {
                        sessions.retain(|saved| saved.agent != agent || saved.id != id)
                    })
                });
            if let Err(err) = changed {
                eprintln!("couldn't drop the deleted session from those kept: {err}");
            }
        }
        // Its agent may have no session on show now.
        self.make_spares(cx);
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
        let session = &self.sessions[self.active];
        if session.busy || text.is_empty() {
            return;
        }
        // A level picked just before goes first, so the prompt runs at it.
        let key = session.key;
        self.send_held(key, cx);
        let session = &mut self.sessions[self.active];
        self.input.update(cx, |input, cx| input.clean(window, cx));
        let first = session.title.is_none();
        session
            .title
            .get_or_insert_with(|| text.lines().next().unwrap_or_default().to_string());
        session.busy = true;
        session.push(Row::User(text.clone()), cx);
        // Live from after the prompt's own row, which shows at once as it was typed here.
        session.set_live(true, cx);
        let key = session.key;
        // Has the agent create the session first, if it hasn't yet.
        let prompt = Command::Prompt(key, text);
        let _ = self.agents[session.agent].commands.try_send(prompt);
        // A spare the agent created has its id already; any other session is kept
        // once the agent gives it one.
        if first {
            self.save_session(key);
        }
        // The window's first prompt lets the agents that need one make a spare.
        self.make_spares(cx);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let session = &mut self.sessions[self.active];
        let _ = self.agents[session.agent]
            .commands
            .try_send(Command::Cancel(session.key));
        // What has arrived shows at once, and permission requests held back behind it
        // show as cancelled: stopping makes acp-core answer the turn's open ones so.
        session.flush(cx);
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

    /// Shows `value` for the session's setting at once, and asks the agent for it once the
    /// user has left it for `HOLD_PAUSE`, or sooner if `send_held` is called first. The
    /// effort meter moves a level for each notch of a wheel, and the agent should get one
    /// change, not one for each level passed.
    fn hold_setting(
        &mut self,
        key: SessionKey,
        setting: String,
        value: SettingValue,
        cx: &mut Context<Self>,
    ) {
        let send = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOLD_PAUSE).await;
            let _ = this.update(cx, |this, cx| this.send_held(key, cx));
        });
        if let Some(session) = self.session_mut(key) {
            session.held = Some(Held {
                setting,
                value,
                _send: send,
            });
        }
        cx.notify();
    }

    /// Asks the agent now for the session's held setting, unless it has come back to the
    /// value it had. Like a switch, it shows the value asked for until the agent answers.
    fn send_held(&mut self, key: SessionKey, cx: &mut Context<Self>) {
        let Some(held) = self
            .session_mut(key)
            .and_then(|session| session.held.take())
        else {
            return;
        };
        let Some(session) = self.session(key) else {
            return;
        };
        let agent = session.agent;
        let unchanged = self
            .shown_settings(session)
            .iter()
            .any(|shown| shown.id == held.setting && shown.value == held.value);
        let change = Command::ChangeSetting(key, held.setting.clone(), held.value.clone());
        // With the agent gone, no answer would ever put the setting right.
        if !unchanged
            && self.agents[agent].commands.try_send(change).is_ok()
            && let Some(session) = self.session_mut(key)
        {
            session.ask(held.setting, held.value);
        }
        cx.notify();
    }

    /// Keeps the settings the agent created the session with as its last, here and on
    /// disk, unless they are the ones kept already. Only a new session's first settings
    /// are those: later ones carry the changes made to it, as a loaded session's may.
    fn remember_settings(&mut self, agent: usize, key: SessionKey, settings: &[Setting]) {
        let first = self
            .session_mut(key)
            .is_some_and(|session| !session.created && matches!(session.origin, Origin::New));
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
            AgentEvent::Connected { .. } => self.agents[agent].connected = true,
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
                        self.sessions[self.active].push_error(message, cx);
                    }
                } else if let Some(session) = self.session_mut(key) {
                    // Open for a prompt, which asks the agent to create the session again.
                    session.busy = false;
                    session.push_error(message, cx);
                    session.set_live(false, cx);
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
                let created = matches!(event, SessionEvent::Created(_));
                let mut retitled = false;
                if let Some(session) = self.session_mut(key) {
                    let title = session.title.clone();
                    session.apply(event, cx);
                    retitled = session.title != title;
                    if session.pacing() {
                        self.pace(cx);
                    }
                }
                if created {
                    self.save_session(key);
                }
                if retitled {
                    self.keep_title(key);
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
                self.sessions[ix].push_error(message, cx)
            }
            AgentEvent::Closed => {
                let spare = self.agents[agent].spare.as_mut();
                for session in self.sessions.iter_mut().chain(spare) {
                    if session.agent != agent {
                        continue;
                    }
                    // A turn cut off with the connection may never report its end, nor
                    // a setting change its answer. Flushed first: the turn's end, if it
                    // came, would open the session to prompts again.
                    session.flush(cx);
                    session.busy = true;
                    session.set_live(false, cx);
                    session.cancel_open_permissions(cx);
                    session.asked.clear();
                    session.held = None;
                }
            }
        }
        cx.notify();
    }

    /// Has the sessions show the text held back in their live turns a piece at a time,
    /// until none holds any.
    fn pace(&mut self, cx: &mut Context<Self>) {
        if self.pacing.is_some() {
            return;
        }
        self.pacing = Some(cx.spawn(async move |this, cx| {
            let mut last = Instant::now();
            loop {
                cx.background_executor().timer(PACE_TICK).await;
                let now = Instant::now();
                let dt = now - last;
                last = now;
                let holding = this.update(cx, |this, cx| {
                    let (mut holding, mut changed) = (false, false);
                    for (ix, session) in this.sessions.iter_mut().enumerate() {
                        let paced = session.pace(dt, cx);
                        holding |= paced.holds;
                        // Text shown off screen draws nothing; a landed event may change
                        // what the window shows of the session anywhere, as its busy state.
                        changed |= paced.landed || paced.shown && ix == this.active;
                    }
                    if !holding {
                        this.pacing = None;
                    }
                    if changed {
                        cx.notify();
                    }
                    holding
                });
                if !matches!(holding, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Answers the permission request on the tool call at `ix` of the session with an
    /// option's id, or `None` to reject it.
    fn answer(
        &mut self,
        key: SessionKey,
        ix: usize,
        choice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session_mut(key) else {
            return;
        };
        let held = match session.rows.get(ix) {
            Some(Row::Tool(call)) => call
                .ask
                .as_ref()
                .is_some_and(|ask| ask.focus.iter().any(|focus| focus.is_focused(window))),
            _ => false,
        };
        session.answer(ix, choice, cx);
        // The keys go back to the prompt from a card that held them, as it folds away.
        if held {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// The sidebar's color and the chat's, which let the blur through while the window
    /// is blurred. The settings' list of pages and their page take the same two.
    fn panel_colors(&self, cx: &App) -> (Hsla, Hsla) {
        let theme = cx.theme();
        if self.blurred {
            (
                theme.sidebar.opacity(BLURRED_SIDEBAR_ALPHA),
                theme.background.opacity(BLURRED_CHAT_ALPHA),
            )
        } else {
            (theme.sidebar, theme.background)
        }
    }
}

/// The sidebar button's icon: a window outline with a short bar near its right side.
const SIDEBAR_ICON: &[u8] = include_bytes!("../assets/sidebar.svg");
/// The sidebar button's width and height.
const SIDEBAR_BUTTON_SIZE: Pixels = px(28.);

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // In full screen the traffic lights go back to the system's title bar, which
        // slides down over the window, so the window keeps none of its own.
        let title_bar = OWN_TITLE_BAR && !window.is_fullscreen();
        let motion = cx.theme().motion_tokens();
        let fade = Transition::new(motion.duration_fast).easing(motion.easing_enter.clone());
        // 1 while the settings are open, 0 while not. The settings fade in on their own,
        // as each page does; the sidebar and the chat fade back in on this.
        let settings_shown = transition(
            "settings-shown",
            if self.settings_page.open { 1. } else { 0. },
            fade,
            window,
            cx,
        );
        if self.settings_page.open {
            return self.render_settings(title_bar, cx);
        }
        let chat_shown = 1. - settings_shown;
        let (sidebar, background) = self.panel_colors(cx);
        let theme = cx.theme();
        let (foreground, input_border) = (theme.foreground, theme.input);
        let (muted, hover) = (theme.muted_foreground, theme.secondary);
        let motion = theme.motion_tokens();
        let slide = Transition::new(motion.duration_slow).easing(motion.easing_move.clone());
        // 0 while the sidebar is shown and 1 while it is hidden, in between as it slides.
        let hidden = transition(
            "sidebar-hidden",
            if self.sidebar.open { 0. } else { 1. },
            slide,
            window,
            cx,
        );
        let sidebar_width = self.sidebar.width * (1. - hidden);
        let transcript = self.render_transcript(window, cx);
        let session = &self.sessions[self.active];
        // The model, how hard it thinks, then the mode, each once the agent has said
        // what it can be, for this session or the last it created.
        let shown = self.shown_settings(session);
        let pickers = [
            self.render_model_picker(session, &shown, cx),
            shown
                .iter()
                .find(|setting| setting.kind == SettingKind::ThoughtLevel)
                .map(|setting| self.render_effort_picker(session.key, setting, window, cx)),
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
            // The chat's color stays as the settings close, so the window doesn't show
            // through while what is on it fades back in.
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
                    .opacity(chat_shown)
                    .when(title_bar, as_title_bar),
            )
            .child(transcript.flex_1().min_h(px(0.)).opacity(chat_shown))
            .child(
                // A row along the input box's bottom: the session's model and mode on the
                // left, the send/stop button on the right.
                div().p_3().opacity(chat_shown).child(
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
            .aria_label(if self.sidebar.open {
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
            .opacity(chat_shown)
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
                this.sidebar.open = !this.sidebar.open;
                cx.notify();
            }));

        div()
            .size_full()
            .relative()
            .flex()
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
                    this.resize_sidebar(event, cx);
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
                // Its color stops here, so that while the window is blurred it isn't
                // also under the chat's.
                div()
                    .flex_none()
                    .h_full()
                    .w(sidebar_width)
                    .overflow_hidden()
                    .bg(sidebar)
                    .child(
                        self.render_sidebar(title_bar, cx)
                            .ml(sidebar_width - self.sidebar.width)
                            .opacity(chat_shown),
                    ),
            )
            .child(chat)
            .child(toggle)
            .into_any_element()
    }
}
