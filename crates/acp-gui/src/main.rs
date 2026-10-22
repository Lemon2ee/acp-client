//! Minimal gpui front end: a sidebar of sessions beside a chat, driving one ACP agent.
//! Usage: `acp-gui [claude | codex | "<agent command>"]`.

use std::borrow::Cow;
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use acp_core::{
    AcpAgent, Command, Event as AgentEvent, PermissionRequest, Sender, SessionEvent, SessionKey,
    Setting, SettingChoice, SettingKind, SettingValue,
};
use gpui_kit::base::{ElementExt as _, Transition, transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    InputEvent, InputGroup, InputGroupAddon, InputGroupAddonAlignment, InputGroupButton, Textarea,
    TextareaState,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, FocusableExt as _, Icon, IconName, Sizable as _, Theme, ThemeConfig,
    ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

enum Row {
    User(String),
    Agent(String),
    Thought(String),
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

/// One conversation with the agent: its transcript and whether it takes a prompt.
struct Session {
    key: SessionKey,
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
}

impl Session {
    fn new(key: SessionKey, cx: &mut App) -> Self {
        Self {
            key,
            title: None,
            rows: Vec::new(),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            busy: true,
            draft: SharedString::default(),
            settings: Vec::new(),
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
            SessionEvent::Settings(settings) => self.settings = settings,
            SessionEvent::ModeChanged(mode) => {
                for setting in &mut self.settings {
                    if setting.kind == SettingKind::Mode {
                        setting.value = SettingValue::Choice(mode.clone());
                    }
                }
            }
            SessionEvent::SettingFailed(message) => self.push(Row::Error(message), cx),
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
        match self.rows.last_mut() {
            Some(Row::Agent(existing)) if !thought => existing.push_str(text),
            Some(Row::Thought(existing)) if thought => existing.push_str(text),
            // Agents often open a block with a bare "\n\n" chunk; don't start a row with it.
            _ if text.trim().is_empty() => return,
            _ => {
                let row = if thought {
                    Row::Thought(text.into())
                } else {
                    Row::Agent(text.into())
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

struct ChatView {
    /// Never empty: the window starts with one session and none are removed.
    sessions: Vec<Session>,
    /// Index into `sessions` of the one on screen.
    active: usize,
    next_key: u64,
    input: Entity<TextareaState>,
    commands: Sender<Command>,
    /// The window's top row, which holds the traffic lights on macOS and the sidebar
    /// button: across the sidebar, or across the chat while the sidebar is hidden.
    title_bar_height: Pixels,
    /// Where the traffic lights end, from the window's left edge.
    traffic_lights_end: Pixels,
    sidebar_open: bool,
    model_menu: ModelMenuState,
    /// Older Claude models the user added to the model list, by id. Kept on disk.
    added_models: Vec<String>,
    _events: Task<()>,
    _input_events: Subscription,
}

impl ChatView {
    fn new(
        agent: AcpAgent,
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

        let agent = acp_core::spawn(agent, cwd);
        let events = agent.events;
        let events_task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update(cx, |this, cx| this.apply(event, cx)).is_err() {
                    break;
                }
            }
        });

        let mut view = Self {
            sessions: Vec::new(),
            active: 0,
            next_key: 0,
            input,
            commands: agent.commands,
            title_bar_height,
            traffic_lights_end,
            sidebar_open: true,
            model_menu: ModelMenuState::default(),
            added_models: load_added_models(),
            _events: events_task,
            _input_events: input_events,
        };
        view.new_session(window, cx);
        view
    }

    /// Adds a session, shows it, and asks the agent to create it. The window's first
    /// session is added even if the agent is already gone, so there is always one to show.
    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.commands.is_closed() && !self.sessions.is_empty() {
            return;
        }
        let key = SessionKey(self.next_key);
        self.next_key += 1;
        self.sessions.push(Session::new(key, cx));
        let _ = self.commands.try_send(Command::NewSession(key));
        self.show(self.sessions.len() - 1, window, cx);
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

    fn session_mut(&mut self, key: SessionKey) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|session| session.key == key)
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
        let _ = self.commands.try_send(Command::Prompt(session.key, text));
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let session = &mut self.sessions[self.active];
        let _ = self.commands.try_send(Command::Cancel(session.key));
        // Stopping makes acp-core answer the turn's open permission requests as cancelled.
        session.cancel_open_permissions(cx);
        cx.notify();
    }

    /// The setting shows its old value until the agent confirms the change.
    fn change_setting(&mut self, key: SessionKey, setting: String, value: SettingValue) {
        let _ = self
            .commands
            .try_send(Command::ChangeSetting(key, setting, value));
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

    /// Adds an older model to the model list, or takes it off again.
    fn set_model_added(&mut self, id: String, added: bool, cx: &mut Context<Self>) {
        self.added_models.retain(|model| *model != id);
        if added {
            self.added_models.push(id);
        }
        save_added_models(&self.added_models);
        cx.notify();
    }

    fn apply(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Connected { .. } => {}
            AgentEvent::SessionReady(key) => {
                if let Some(session) = self.session_mut(key) {
                    session.busy = false;
                }
            }
            AgentEvent::SessionFailed(key, message) => {
                if let Some(session) = self.session_mut(key) {
                    // Open for a prompt, which asks the agent to create the session again.
                    session.busy = false;
                    session.push(Row::Error(message), cx);
                }
            }
            AgentEvent::Session(key, event) => {
                if let Some(session) = self.session_mut(key) {
                    session.apply(event, cx);
                }
            }
            // Connection-wide failures show in the session on screen.
            AgentEvent::Error(message) => self.sessions[self.active].push(Row::Error(message), cx),
            AgentEvent::Closed => {
                for session in &mut self.sessions {
                    session.busy = true;
                    // A turn cut off with the connection may never report its end.
                    session.cancel_open_permissions(cx);
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
        let (muted, border, bubble, danger) = (
            theme.muted_foreground,
            theme.border,
            theme.secondary,
            theme.danger,
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
                        .child(SharedString::from(text.clone())),
                )
                .into_any_element(),
            Row::Agent(text) => row
                .child(SharedString::from(text.trim().to_string()))
                .into_any_element(),
            Row::Thought(text) => row
                .text_sm()
                .italic()
                .text_color(muted)
                .child(SharedString::from(text.trim().to_string()))
                .into_any_element(),
            Row::Tool { title, status, .. } => row
                .text_sm()
                .text_color(muted)
                .child(format!("tool: {title} ({status})"))
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
                    .child(format!("Permission requested: {}", request.title));
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
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            Row::Error(text) => row
                .text_sm()
                .text_color(danger)
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        }
    }
}

const SIDEBAR_WIDTH: f32 = 240.;

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

/// What the parts of the model menu share.
struct ModelMenu<'a> {
    view: &'a WeakEntity<ChatView>,
    key: SessionKey,
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
                let _ = view.update(cx, |this, _| {
                    this.change_setting(key, id.clone(), SettingValue::Choice(value.clone()))
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
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
/// the menu.
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
    // Where the open lists are drawn, so a press in them leaves the menu open, and the
    // pointer's way to them can be told. Set as they are drawn; a closed list has none.
    let (list_bounds, older_bounds) = (menu.list_bounds.clone(), menu.older_bounds.clone());
    list_bounds.set(None);
    older_bounds.set(None);

    let rows = settings.iter().map(|setting| {
        let list = (!setting.choices.is_empty()).then(|| setting.id.clone());
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
                let (view, key, id, on) = (menu.view.clone(), menu.key, setting.id.clone(), *on);
                row.child(
                    // Only shows the value: a press anywhere on the row flips it.
                    Switch::new(ElementId::Name(
                        format!("model-menu-switch-{}", setting.id).into(),
                    ))
                    .checked(on)
                    .tab_stop(false),
                )
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, _| {
                        this.change_setting(key, id.clone(), SettingValue::Switch(!on))
                    });
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
        .filter(|&ix| !settings[ix].choices.is_empty())
        .map(|ix| {
            let setting = &settings[ix];
            let current = shown_choice(setting).map(|choice| choice.id.as_str());
            let claude = (setting.kind == SettingKind::Model)
                .then(|| claude_models(&setting.choices, added))
                .flatten();
            let rows = match &claude {
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
            let rows = rows
                .into_iter()
                .map(|row| {
                    row.on_hover(menu.wants(Some(setting.id.clone()), false))
                        .into_any_element()
                })
                .collect();
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
            frame.when_some(older, |frame, models| {
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
                    // Level with the row that opens it, the last of this list.
                    menu.list(
                        "model-menu-older",
                        rows,
                        px(0.),
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

impl ChatView {
    /// The button for the model and how hard it thinks, named like "Opus 5.5 High",
    /// once the agent offers either. Its menu opens upward: a switch for each of the
    /// model's settings that are on or off, then a row for each of its others, whose
    /// choices open beside it.
    fn render_model_picker(&self, session: &Session, cx: &mut Context<Self>) -> Option<AnyElement> {
        let names: Vec<&str> = [SettingKind::Model, SettingKind::ThoughtLevel]
            .into_iter()
            .filter_map(|kind| {
                shown_choice(
                    session
                        .settings
                        .iter()
                        .find(|setting| setting.kind == kind)?,
                )
            })
            .map(|choice| choice.name.as_str())
            .collect();
        if names.is_empty() {
            return None;
        }
        let label = names.join(" ");
        let settings: Vec<Setting> = [
            SettingKind::ModelConfig,
            SettingKind::ThoughtLevel,
            SettingKind::Model,
        ]
        .into_iter()
        .flat_map(|kind| {
            session
                .settings
                .iter()
                .filter(move |setting| setting.kind == kind)
        })
        .cloned()
        .collect();
        let view = cx.entity().downgrade();
        let key = session.key;
        let open_list = self.model_menu.list.clone();
        let older_open = self.model_menu.older;
        let list_bounds = self.model_menu.list_bounds.clone();
        let older_bounds = self.model_menu.older_bounds.clone();
        let added = self.added_models.clone();
        let trigger = InputGroupButton::new("model-picker")
            .label(label.clone())
            .accessibility_label(format!("Model: {label}"))
            .dropdown_caret(true)
            .small()
            .cursor_pointer();
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
        let current = setting.chosen();
        let name = current.map_or_else(|| setting.name.clone(), |choice| choice.name.clone());
        let icon = mode_icon(current.map_or("", |choice| choice.id.as_str()));
        let view = cx.entity().downgrade();
        let setting = setting.clone();
        InputGroupButton::new("mode-picker")
            .icon(icon)
            .label(name.clone())
            .accessibility_label(format!("{}: {name}", setting.name))
            .dropdown_caret(true)
            .small()
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
        let (foreground, hover, divider) = (
            theme.sidebar_foreground,
            theme.secondary,
            theme.sidebar_border,
        );
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
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            // The line between the sidebar and the chat, which goes with the sidebar.
            .border_r_1()
            .border_color(divider)
            .text_color(foreground)
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
        let sidebar_width = SIDEBAR_WIDTH * (1. - hidden);
        let session = &self.sessions[self.active];
        // The model and how hard it thinks, then the mode, each once the agent has said
        // what it can be.
        let pickers = [
            self.render_model_picker(session, cx),
            session
                .settings
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
            .child(
                // The sidebar keeps its width and slides out to the left as this narrows.
                div()
                    .flex_none()
                    .h_full()
                    .w(px(sidebar_width))
                    .overflow_hidden()
                    .child(
                        self.render_sidebar(title_bar, cx)
                            .ml(px(sidebar_width - SIDEBAR_WIDTH)),
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

/// Where the older models the user added to the model list are kept, an id a line.
fn added_models_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("acp-client").join("added-models"))
}

fn load_added_models() -> Vec<String> {
    let text = added_models_path().and_then(|path| std::fs::read_to_string(path).ok());
    text.map(|text| {
        text.lines()
            .filter(|line| !line.is_empty())
            .map(Into::into)
            .collect()
    })
    .unwrap_or_default()
}

/// The list still changes for this run when saving it fails.
fn save_added_models(models: &[String]) {
    let Some(path) = added_models_path() else {
        return;
    };
    let text: String = models.iter().map(|id| format!("{id}\n")).collect();
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, text));
    if let Err(err) = saved {
        eprintln!(
            "couldn't save the added models to {}: {err}",
            path.display()
        );
    }
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

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "claude".into());
    let agent = match acp_core::agent_for(&name) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("bad agent command: {err}");
            std::process::exit(2);
        }
    };
    let cwd = std::env::current_dir().expect("no current directory");

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
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
                    title: Some(format!("ACP client: {name}").into()),
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
                cx.new(|cx| ChatView::new(agent, cwd, lights.row_height, lights_end, window, cx))
            })
            .expect("failed to open the window");
            cx.activate(true);
        });
}
