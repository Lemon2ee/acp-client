//! Minimal gpui front end: a sidebar of sessions beside a chat, driving one ACP agent.
//! Usage: `acp-gui [claude | codex | "<agent command>"]`.

use std::path::PathBuf;
use std::rc::Rc;

use acp_core::{
    AcpAgent, Command, Event as AgentEvent, PermissionRequest, Sender, SessionEvent, SessionKey,
};
use gpui_kit::base::{Transition, transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    InputEvent, InputGroup, InputGroupAddon, InputGroupAddonAlignment, InputGroupButton, Textarea,
    TextareaState,
};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::{
    ActiveTheme as _, FocusableExt as _, Icon, IconName, Sizable as _, Theme, ThemeConfig,
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

impl ChatView {
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
                // The send/stop button sits inside the input box.
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
                        // 129px tall until the text needs more room, with the text at the top.
                        .input(Textarea::new(&self.input).min_h(px(129.)))
                        .addon(
                            InputGroupAddon::new("prompt-actions")
                                .align(InputGroupAddonAlignment::InlineEnd)
                                // Keep the button in the bottom corner as the box grows.
                                .self_end()
                                .child(action.primary().small().rounded_full()),
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
        .with_assets(gpui_kit::assets::Assets)
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
