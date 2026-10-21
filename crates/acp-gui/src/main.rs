//! Minimal gpui front end: a message list and an input box, driving one ACP agent.
//! Usage: `acp-gui [claude | codex | "<agent command>"]`.

use std::path::PathBuf;

use acp_core::{AcpAgent, Command, Event as AgentEvent, PermissionRequest, Sender};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::*;

enum Row {
    User(String),
    Agent(String),
    Thought(String),
    Tool { id: String, title: String, status: String },
    Permission { request: PermissionRequest, decided: Option<String> },
    Notice(String),
    Error(String),
}

struct ChatView {
    rows: Vec<Row>,
    scroller: Entity<MessageScrollerState>,
    input: Entity<TextareaState>,
    commands: Sender<Command>,
    status: SharedString,
    /// Not ready for a prompt: still connecting, a turn is running, or the agent is gone.
    busy: bool,
    _events: Task<()>,
    _input_events: Subscription,
}

impl ChatView {
    fn new(agent: AcpAgent, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message the agent (Enter to send, Shift+Enter for a new line)")
                .auto_grow(1, 8)
                .submit_on_enter(true)
        });
        input.update(cx, |input, cx| input.focus(window, cx));
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

        Self {
            rows: Vec::new(),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            input,
            commands: agent.commands,
            status: "starting the agent...".into(),
            busy: true,
            _events: events_task,
            _input_events: input_events,
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if self.busy || text.is_empty() {
            return;
        }
        self.input.update(cx, |input, cx| input.clean(window, cx));
        self.busy = true;
        self.push(Row::User(text.clone()), cx);
        let _ = self.commands.try_send(Command::Prompt(text));
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let _ = self.commands.try_send(Command::Cancel);
        cx.notify();
    }

    fn apply(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Ready { agent } => {
                self.status = format!("connected: {agent}").into();
                self.busy = false;
            }
            AgentEvent::AgentText(text) => self.append_text(false, &text, cx),
            AgentEvent::AgentThought(text) => self.append_text(true, &text, cx),
            AgentEvent::ToolCall { id, title, status } => {
                self.push(Row::Tool { id, title, status }, cx)
            }
            AgentEvent::ToolCallUpdate { id, title, status } => {
                let found = self.rows.iter_mut().enumerate().rev().find_map(|(ix, row)| match row {
                    Row::Tool { id: row_id, title: row_title, status: row_status } if *row_id == id => {
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
            AgentEvent::Plan(entries) => self.push(Row::Notice(format!("plan\n{}", entries.join("\n"))), cx),
            AgentEvent::Permission(request) => self.push(Row::Permission { request, decided: None }, cx),
            AgentEvent::TurnEnded { stop_reason } => {
                self.busy = false;
                if stop_reason != "EndTurn" {
                    self.push(Row::Notice(format!("turn ended: {stop_reason}")), cx);
                }
            }
            AgentEvent::Error(message) => self.push(Row::Error(message), cx),
            AgentEvent::Closed => {
                self.status = "agent disconnected".into();
                self.busy = true;
            }
        }
        cx.notify();
    }

    fn answer(&mut self, ix: usize, choice: Option<String>, cx: &mut Context<Self>) {
        if let Some(Row::Permission { request, decided }) = self.rows.get_mut(ix) {
            let label = choice
                .as_ref()
                .and_then(|id| request.options.iter().find(|option| option.id == *id))
                .map_or_else(|| "rejected".to_string(), |option| option.name.clone());
            request.answer(choice);
            *decided = Some(label);
            self.remeasure(ix, cx);
            cx.notify();
        }
    }

    /// Streams arrive as small chunks; grow the last row instead of adding one per chunk.
    fn append_text(&mut self, thought: bool, text: &str, cx: &mut Context<Self>) {
        match self.rows.last_mut() {
            Some(Row::Agent(existing)) if !thought => existing.push_str(text),
            Some(Row::Thought(existing)) if thought => existing.push_str(text),
            // Agents often open a block with a bare "\n\n" chunk; don't start a row with it.
            _ if text.trim().is_empty() => return,
            _ => {
                let row = if thought { Row::Thought(text.into()) } else { Row::Agent(text.into()) };
                return self.push(row, cx);
            }
        }
        self.remeasure(self.rows.len() - 1, cx);
    }

    fn push(&mut self, row: Row, cx: &mut Context<Self>) {
        self.rows.push(row);
        self.scroller.update(cx, |scroller, cx| {
            scroller.append(1, cx);
        });
    }

    fn remeasure(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.scroller.update(cx, |scroller, cx| {
            scroller.remeasure_items(ix..ix + 1, cx);
        });
    }

    fn render_row(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, bubble) = (theme.muted_foreground, theme.border, theme.secondary);
        let row = div().w_full().px_4().py_1p5();
        match &self.rows[ix] {
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
            Row::Agent(text) => row.child(SharedString::from(text.trim().to_string())).into_any_element(),
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
                    Some(label) => card.child(div().text_sm().text_color(muted).child(format!("answered: {label}"))),
                    None => card.child(div().flex().gap_2().children(
                        request.options.iter().enumerate().map(|(option_ix, option)| {
                            let id = option.id.clone();
                            let button = Button::new(ElementId::Name(format!("permission-{ix}-{option_ix}").into()))
                                .label(option.name.clone())
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.answer(ix, Some(id.clone()), cx)
                                }));
                            if option.allow { button.primary() } else { button }
                        }),
                    )),
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
                .text_color(red())
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        }
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, muted, border) =
            (theme.background, theme.foreground, theme.muted_foreground, theme.border);
        let action = if self.busy {
            Button::new("stop")
                .label("Stop")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)))
        } else {
            Button::new("send")
                .primary()
                .label("Send")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx)))
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .text_sm()
                    .text_color(muted)
                    .child(self.status.clone()),
            )
            .child(
                MessageScroller::new(
                    "messages",
                    self.scroller.clone(),
                    cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx)),
                )
                .flex_1()
                .min_h(px(0.)),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(border)
                    .child(div().flex_1().child(Textarea::new(&self.input)))
                    .child(action),
            )
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
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(760.), px(820.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some(format!("ACP client: {name}").into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| ChatView::new(agent, cwd, window, cx))
            })
            .expect("failed to open the window");
            cx.activate(true);
        });
}
