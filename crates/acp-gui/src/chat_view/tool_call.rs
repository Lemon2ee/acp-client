//! A tool call's row in the transcript: its kind's icon, its title and how it stands, and
//! once opened, what it produced or the change it makes, in a box under the title.

use std::cell::Cell;
use std::f32::consts::FRAC_PI_2;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use acp_core::{PermissionChoice, SessionKey, ToolKind};
use gpui_kit::base::actions::{Cancel, Confirm, SelectDown, SelectUp};
use gpui_kit::base::{ElementExt as _, Transition, spring, transition};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use similar::ChangeTag;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::MenuLook;
use crate::session::{
    Answer, Ask, Code, Diff, DiffLine, Row, Shown, ToolCall, folded_text, row_id,
};

/// A row is as tall as the menus' rows and sits a little in from the transcript's sides,
/// its padding putting the icon in line with the agent's text.
const ROW_HEIGHT: Pixels = px(28.);
const ROW_INSET: Pixels = px(10.);
const ROW_PAD: Pixels = px(6.);
const ROW_RADIUS: Pixels = px(6.);
/// Between a row or a group and what comes before or after it.
const ROW_GAP: Pixels = px(4.);
/// How far a group's calls sit in from its header, which puts their icons under its title.
const GROUP_INDENT: Pixels = px(22.);
const ROW_LINE_HEIGHT: Pixels = px(20.);
const ICON_SIZE: Pixels = px(14.);
const CHEVRON_SIZE: Pixels = px(12.);
/// Between the icon, the title and the chevron.
const ICON_GAP: Pixels = px(8.);
/// A command's or a search's title is the command itself, set in the monospace face.
const MONO_TITLE_SIZE: Pixels = px(13.);
/// The counts, the words and the time at the row's end, and the box's header.
const NOTE_SIZE: Pixels = px(12.);
const NOTE_LINE_HEIGHT: Pixels = px(16.);
/// The box with what a call shows starts under its title: past the row's inset, its
/// padding, the icon and the gap after it.
const BOX_LEFT: Pixels = px(38.);
const BOX_RIGHT: Pixels = px(16.);
const BOX_RADIUS: Pixels = px(8.);
const BOX_HEADER_HEIGHT: Pixels = px(28.);
const BOX_TEXT_SIZE: Pixels = px(14.);
const BOX_TEXT_LINE_HEIGHT: Pixels = px(21.);
pub(super) const CODE_SIZE: Pixels = px(12.);
pub(super) const CODE_LINE_HEIGHT: Pixels = px(18.);
const CODE_PAD: Pixels = px(10.);
const NUMBER_WIDTH: Pixels = px(28.);
const SIGN_WIDTH: Pixels = px(12.);
/// At most twelve lines of code show at once; the rest scroll inside the box.
const BOX_LINES: usize = 12;
/// The same height for text drawn as markdown.
const BOX_MAX_HEIGHT: Pixels = px(216.);
/// Over the bottom of a box that scrolls, while more is below.
const BOX_FADE: Pixels = px(28.);
const COPY_INSET: Pixels = px(6.);
/// The permission card, the menus' surface: its padding inside, and how far it sits
/// from the rows above and below it.
const CARD_PAD: Pixels = px(6.);
const CARD_ABOVE: Pixels = px(4.);
const CARD_BELOW: Pixels = px(8.);
/// The card's question and each of its options, as tall as the menus' roomier rows.
const CARD_ROW_HEIGHT: Pixels = px(32.);
const CARD_ROW_PAD: Pixels = px(10.);
const QUESTION_SIZE: Pixels = px(14.);
/// Around the preview of what the call would do, inside the card.
const PREVIEW_INSET: Pixels = px(4.);
const PREVIEW_BELOW: Pixels = px(6.);
/// How long a running call goes before its row counts the seconds.
const COUNT_AFTER: Duration = Duration::from_secs(1);
/// How often the count goes up.
const COUNT_TICK: Duration = Duration::from_secs(1);
/// The row's hover group, which brightens its text and icons with its background.
const ROW_GROUP: &str = "tool-call-row";
/// The box's hover group, which shows its copy button.
const BOX_GROUP: &str = "tool-call-box";

/// The colors of the tool calls' rows and boxes, quieter than the agent's text. The
/// folder line's counts of lines a branch added and removed take theirs from here too.
#[derive(Clone, Copy)]
pub(super) struct ToolLook {
    /// The title and the icon.
    muted: Hsla,
    /// The title and the icon under the pointer.
    bright: Hsla,
    /// Behind a row under the pointer, and while it is pressed.
    hover: Hsla,
    pressed: Hsla,
    /// The chevron and the notes at the row's end.
    faint: Hsla,
    failed: Hsla,
    pub(super) added: Hsla,
    added_bg: Hsla,
    pub(super) removed: Hsla,
    removed_bg: Hsla,
    /// The box's fill, its edge and its text.
    code: Hsla,
    line: Hsla,
    pub(super) code_text: Hsla,
    /// Behind the copy button where it floats over the box's text.
    card: Hsla,
}

impl ToolLook {
    pub(super) fn new(cx: &App) -> Self {
        let color = |hex: u32| Hsla::from(rgb(hex));
        if cx.theme().is_dark() {
            Self {
                muted: color(0xa3a3a3),
                bright: color(0xd4d4d4),
                hover: white().opacity(0.05),
                pressed: white().opacity(0.08),
                faint: color(0x8f8f8f),
                failed: color(0xf87171),
                added: color(0x4ade80),
                added_bg: color(0x4ade80).opacity(0.12),
                removed: color(0xf87171),
                removed_bg: color(0xf87171).opacity(0.12),
                code: color(0x1b1b1b),
                line: white().opacity(0.08),
                code_text: color(0xd4d4d4),
                card: color(0x262626),
            }
        } else {
            Self {
                muted: color(0x6b6b6b),
                bright: color(0x262626),
                hover: black().opacity(0.04),
                pressed: black().opacity(0.07),
                faint: color(0x737373),
                failed: color(0xdc2626),
                added: color(0x15803d),
                added_bg: color(0x16a34a).opacity(0.10),
                removed: color(0xb91c1c),
                removed_bg: color(0xdc2626).opacity(0.09),
                code: color(0xf5f5f5),
                line: black().opacity(0.08),
                code_text: color(0x262626),
                card: white(),
            }
        }
    }
}

impl ChatView {
    /// The tool call at `ix` of the active session's transcript. A call with others right
    /// before or after it is drawn in their group: the first under the group's header, all
    /// of them set in under it and shown only while the group is open.
    pub(crate) fn render_tool_call(
        &mut self,
        ix: usize,
        keyed: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let motion = cx.theme().motion_tokens();
        let (control, normal, enter) = (
            motion.spring_control,
            motion.duration_normal,
            motion.easing_enter.clone(),
        );
        let session = &self.sessions[self.active];
        let key = session.key;
        let group = session.tool_group(ix);
        let id = row_id("tool", key, ix);
        let grouped = group.len() > 1;
        // The first call's row makes way for the header as the second call arrives, and
        // each call moves in under it.
        let headed = (ix == group.start).then(|| {
            transition(
                (id.clone(), "headed"),
                if grouped { 1. } else { 0. },
                Transition::new(normal).easing(enter.clone()),
                window,
                cx,
            )
        });
        let indent = transition(
            (id.clone(), "indent"),
            if grouped { GROUP_INDENT.as_f32() } else { 0. },
            Transition::new(normal).easing(enter),
            window,
            cx,
        );
        if !grouped {
            return self
                .render_call(ix, px(indent), keyed, window, cx)
                .py(ROW_GAP)
                .into_any_element();
        }
        let open = session.group_open(&group);
        let group_id = row_id("tool-group", key, group.start);
        let reveal = spring(
            (group_id.clone(), "reveal"),
            if open { 1. } else { 0. },
            control,
            window,
            cx,
        );
        let last = ix + 1 == group.end;
        let header = headed.map(|headed| {
            let header = self.render_group_header(group.clone(), reveal, window, cx);
            div()
                .overflow_hidden()
                .opacity(headed)
                .when(headed < 1., |clip| clip.h(ROW_HEIGHT * headed))
                .child(header)
        });
        let call = (reveal > 0. || open).then(|| {
            let call = self.render_call(ix, px(indent), keyed, window, cx);
            // A line down from the header's icon, along the calls, to the last one's middle.
            let guide = div()
                .absolute()
                .left(ROW_INSET + ROW_PAD + ICON_SIZE / 2.)
                .top_0()
                .w(px(1.))
                .bg(ToolLook::new(cx).line)
                .map(|guide| {
                    if last {
                        guide.h(ROW_HEIGHT / 2.)
                    } else {
                        guide.bottom_0()
                    }
                });
            reveal_clip(
                (id.clone(), "in-group"),
                reveal,
                div().relative().child(guide).child(call),
                window,
                cx,
            )
        });
        v_flex()
            .w_full()
            .when(ix == group.start, |item| item.pt(ROW_GAP))
            .when(last, |item| item.pb(ROW_GAP))
            .children(header)
            .children(call)
            .into_any_element()
    }

    /// One tool call: its row, set in by `indent`, and the box with what it shows, which
    /// opens and closes with the theme's control spring. Its permission card shows the
    /// keys while it is the `keyed` one.
    fn render_call(
        &mut self,
        ix: usize,
        indent: Pixels,
        keyed: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        self.return_keys(ix, window, cx);
        let look = ToolLook::new(cx);
        let motion = cx.theme().motion_tokens();
        let (control, fast) = (motion.spring_control, motion.duration_fast);
        let mono = cx.theme().mono_font_family.clone();
        let session = &self.sessions[self.active];
        let key = session.key;
        let Some(Row::Tool(call)) = session.rows.get(ix) else {
            return div();
        };
        let id = row_id("tool", key, ix);
        let folder = session.folder().to_path_buf();
        // Claude's adapter keeps a call pending until it finishes, sending in_progress only
        // with progress beats on long ones, so any unfinished call in a live turn runs,
        // unless it waits for the user's permission or didn't get it.
        let asking = call.asking();
        let running = session.is_live() && !call.status.finished() && call.may_run();
        // The row and the permission card trade places with the control spring.
        let asked = spring(
            (id.clone(), "asked"),
            if asking { 1. } else { 0. },
            control,
            window,
            cx,
        );
        let counting = call.running_for().filter(|_| running);
        let expandable = !call.content.is_empty();
        let reveal = spring(
            (id.clone(), "reveal"),
            if call.open && expandable { 1. } else { 0. },
            control,
            window,
            cx,
        );
        // The spinner and the kind's icon cross-fade as the call starts or finishes.
        let settled = transition(
            (id.clone(), "settled"),
            if running { 0. } else { 1. },
            Transition::new(fast),
            window,
            cx,
        );
        // A call that never got to run took no time worth showing.
        let time = call
            .took
            .map(took_text)
            .or_else(|| counting.filter(|d| *d >= COUNT_AFTER).map(count_text))
            .filter(|_| call.may_run());
        let time_shown = transition(
            (id.clone(), "time"),
            if time.is_some() { 1. } else { 0. },
            Transition::new(fast),
            window,
            cx,
        );

        let line = h_flex()
            .w_full()
            .min_w_0()
            .gap(ICON_GAP)
            .line_height(ROW_LINE_HEIGHT)
            .text_color(if running || call.status.finished() {
                look.muted
            } else {
                look.faint
            })
            .when(expandable, |line| {
                line.group_hover(ROW_GROUP, move |style| style.text_color(look.bright))
            })
            .child(lead_icon(call, settled, look))
            .child(title(call, running, &mono, &id))
            .when(expandable, |line| line.child(chevron(reveal, look)))
            .child(notes(call, time, time_shown, look, &mono));
        let row = if expandable {
            Button::new(id.clone())
                .custom(
                    ButtonCustomVariant::new(cx)
                        .foreground(look.muted)
                        .hover(look.hover)
                        .active(look.pressed),
                )
                .group(ROW_GROUP)
                .w_full()
                .h(ROW_HEIGHT)
                .px(ROW_PAD)
                .rounded(ROW_RADIUS)
                .accessibility_label(call.title.clone())
                .child(line)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.toggle_tool_call(key, ix, cx)
                }))
                .into_any_element()
        } else {
            div()
                .h(ROW_HEIGHT)
                .px(ROW_PAD)
                .flex()
                .items_center()
                .child(line)
                .into_any_element()
        };

        // The box is only laid out while open or moving: closed, it stays out of the frame.
        let content = (reveal > 0. || call.open && expandable).then(|| {
            let shown = content_box(call, &id, &folder, look, &mono, window, cx);
            reveal_clip((id.clone(), "box"), reveal, shown, window, cx)
        });
        let side = (asked < 1.).then(|| {
            let side = v_flex()
                .w_full()
                .child(div().px(ROW_INSET).child(row))
                .children(content);
            reveal_clip((id.clone(), "row"), 1. - asked, side, window, cx)
        });
        let keyed = keyed == Some(ix);
        let card = call.ask.as_ref().filter(|_| asked > 0.).map(|ask| {
            let card = ask_card(
                call, ask, keyed, key, ix, &id, &folder, look, &mono, window, cx,
            );
            reveal_clip((id.clone(), "card"), asked, card, window, cx)
        });
        let element = v_flex().w_full().pl(indent).children(side).children(card);
        if counting.is_some() {
            self.keep_tool_clock(cx);
        }
        element
    }

    /// A group's header: what its calls did, as "Ran 2 commands, read 1 file", how many
    /// failed and how long the group took. It opens and closes the group.
    fn render_group_header(
        &mut self,
        group: Range<usize>,
        reveal: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let look = ToolLook::new(cx);
        let fast = cx.theme().motion_tokens().duration_fast;
        let session = &self.sessions[self.active];
        let key = session.key;
        let live = session.is_live();
        let calls: Vec<&ToolCall> = session.rows[group.clone()]
            .iter()
            .filter_map(|row| match row {
                Row::Tool(call) => Some(call),
                _ => None,
            })
            .collect();
        let waiting = calls.iter().any(|call| call.asking());
        let running = live
            && calls
                .iter()
                .any(|call| !call.status.finished() && call.may_run());
        let id = row_id("tool-group", key, group.start);
        let settled = transition(
            (id.clone(), "settled"),
            if running { 0. } else { 1. },
            Transition::new(fast),
            window,
            cx,
        );
        // The call asking for permission says so on its card.
        let summary: SharedString = summary(
            calls
                .iter()
                .filter(|call| !call.asking())
                .map(|call| (call.kind, live && !call.status.finished() && call.may_run())),
        )
        .into();
        let failed = calls.iter().filter(|call| call.failed()).count();
        let time = group_time(&calls, running).filter(|_| !waiting);
        let time_shown = transition(
            (id.clone(), "time"),
            if time.is_some() { 1. } else { 0. },
            Transition::new(fast),
            window,
            cx,
        );

        let line = h_flex()
            .w_full()
            .min_w_0()
            .gap(ICON_GAP)
            .line_height(ROW_LINE_HEIGHT)
            .text_color(look.muted)
            .group_hover(ROW_GROUP, move |style| style.text_color(look.bright))
            .child(lead(Icon::new(gpui_kit::assets::IconName::Layers), settled))
            .child(div().min_w_0().flex_initial().truncate().map(|title| {
                if running {
                    title.child(ShimmerText::new(summary.clone()).id((id.clone(), "summary")))
                } else {
                    title.child(summary.clone())
                }
            }))
            .child(chevron(reveal, look))
            .child(
                h_flex()
                    .ml_auto()
                    .pl_3()
                    .flex_none()
                    .gap_2()
                    .text_size(NOTE_SIZE)
                    .line_height(NOTE_LINE_HEIGHT)
                    .text_color(look.faint)
                    .when(failed > 0, |notes| {
                        notes.child(
                            div()
                                .text_color(look.failed)
                                .child(format!("{failed} failed")),
                        )
                    })
                    .when(waiting, |notes| {
                        notes.child(div().text_color(look.bright).child("Waiting for you"))
                    })
                    .when_some(time, |notes, time| {
                        notes.child(div().opacity(time_shown).child(time))
                    }),
            );
        let first = group.start;
        let header = Button::new(id)
            .custom(
                ButtonCustomVariant::new(cx)
                    .foreground(look.muted)
                    .hover(look.hover)
                    .active(look.pressed),
            )
            .group(ROW_GROUP)
            .w_full()
            .h(ROW_HEIGHT)
            .px(ROW_PAD)
            .rounded(ROW_RADIUS)
            .accessibility_label(summary)
            .child(line)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_tool_group(key, first, cx)
            }));
        if running {
            self.keep_tool_clock(cx);
        }
        div().px(ROW_INSET).child(header)
    }

    /// The permission card that Enter, Esc, Up and Down answer from the prompt: the
    /// session's first open one, while the prompt holds the keys with nothing written in
    /// it and the newest rows are in view. The card never takes the keys itself, so text
    /// being written can't answer it, nor can a card out of sight. It reads the
    /// transcript's list, so the rows get it from `render_transcript` rather than ask.
    pub(crate) fn card_with_keys(&self, window: &Window, cx: &App) -> Option<usize> {
        let prompt = self.input.read(cx);
        let session = &self.sessions[self.active];
        if !prompt.focus_handle(cx).is_focused(window)
            || !prompt.value().is_empty()
            || session.scroller.read(cx).scrolled_up()
        {
            return None;
        }
        session
            .rows
            .iter()
            .position(|row| matches!(row, Row::Tool(call) if call.asking()))
    }

    /// Takes a key pressed anywhere in the window for a permission card, before the
    /// window's own shortcuts: from the prompt, the keys `card_with_keys` names; on a
    /// card's option, reached with Tab, a key that types something, which goes back to
    /// the prompt with the keys so writing there goes on. Returns whether it was taken.
    pub(crate) fn card_keystroke(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = keystroke.modifiers;
        // macOS sets fn on its own for the arrows.
        if modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
        if self.option_with_keys(window).is_some() {
            let typed = keystroke
                .key_char
                .clone()
                .filter(|text| keystroke.key != "space" && !text.chars().any(char::is_control));
            let Some(text) = typed else {
                return false;
            };
            self.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.insert(&text, window, cx);
            });
            return true;
        }
        if modifiers.shift {
            return false;
        }
        let Some(ix) = self.card_with_keys(window, cx) else {
            return false;
        };
        let session = &mut self.sessions[self.active];
        let key = session.key;
        let Some(Row::Tool(call)) = session.rows.get_mut(ix) else {
            return false;
        };
        let Some(ask) = call.ask.as_mut() else {
            return false;
        };
        let options = &ask.request.options;
        let choice = match keystroke.key.as_str() {
            "up" | "down" if !options.is_empty() => {
                let step = if keystroke.key == "down" {
                    1
                } else {
                    options.len() - 1
                };
                ask.lit = (ask.lit + step) % options.len();
                cx.notify();
                return true;
            }
            "enter" => options.get(ask.lit),
            // Only the agent's own no: answering with none tells it the turn was stopped.
            "escape" => reject_option(options).map(|n| &options[n]),
            _ => None,
        };
        let Some(choice) = choice.map(|option| option.id.clone()) else {
            return false;
        };
        self.answer(key, ix, Some(choice), window, cx);
        true
    }

    /// The card and the option whose own focus holds the keys, reached with Tab.
    fn option_with_keys(&self, window: &Window) -> Option<(usize, usize)> {
        self.sessions[self.active]
            .rows
            .iter()
            .enumerate()
            .find_map(|(ix, row)| match row {
                Row::Tool(call) => {
                    let ask = call.ask.as_ref()?;
                    let n = ask
                        .focus
                        .iter()
                        .position(|focus| focus.is_focused(window))?;
                    Some((ix, n))
                }
                _ => None,
            })
    }

    /// A card that closed with its option holding the keys, as when the turn stopped,
    /// gives them back to the prompt rather than leave them where nothing is drawn.
    fn return_keys(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Tool(call)) = self.sessions[self.active].rows.get(ix) else {
            return;
        };
        let held = call.ask.as_ref().is_some_and(|ask| {
            ask.answer != Answer::Open && ask.focus.iter().any(|focus| focus.is_focused(window))
        });
        if held {
            let input = self.input.clone();
            // Not while the transcript draws, which would leave the frame showing the
            // keys where they were.
            window.defer(cx, move |window, cx| {
                input.update(cx, |input, cx| input.focus(window, cx))
            });
        }
    }

    fn toggle_tool_group(&mut self, key: SessionKey, first: usize, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut(key) {
            session.toggle_tool_group(first, cx);
        }
        cx.notify();
    }

    fn toggle_tool_call(&mut self, key: SessionKey, ix: usize, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut(key) {
            session.toggle_tool_call(ix, cx);
        }
        cx.notify();
    }

    /// Draws the window again each second while the session on screen has a tool call
    /// counting the seconds it runs.
    fn keep_tool_clock(&mut self, cx: &mut Context<Self>) {
        if self.tool_clock.is_some() {
            return;
        }
        self.tool_clock = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(COUNT_TICK).await;
                let counting = this.update(cx, |this, cx| {
                    let session = &this.sessions[this.active];
                    let counting = session.is_live()
                        && session.rows.iter().any(|row| {
                            matches!(row, Row::Tool(call)
                                if call.running_for().is_some() && call.may_run())
                        });
                    if counting {
                        cx.notify();
                    } else {
                        this.tool_clock = None;
                    }
                    counting
                });
                if !matches!(counting, Ok(true)) {
                    break;
                }
            }
        }));
    }
}

/// `child` cut to `reveal` of the height it last laid out at, and faded as much; whole,
/// and its shadows uncut, once `reveal` is 1. Measured here rather than by the library's
/// reveal, which lays content out at no height the frame it first draws, so an open row
/// scrolling into view would jump.
fn reveal_clip(
    id: impl Into<ElementId>,
    reveal: f32,
    child: Div,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let height = window
        .use_keyed_state(id, cx, |_, _| Rc::new(Cell::new(None)))
        .read(cx)
        .clone();
    let measured = height.clone();
    div()
        .opacity(reveal)
        .when(reveal < 1., |clip| {
            clip.overflow_hidden()
                .h(height.get().unwrap_or_default() * reveal)
        })
        .child(child.on_prepaint(move |bounds, _, _| measured.set(Some(bounds.size.height))))
}

/// The icon at the row's start: a spinner while the call runs, a cross once it failed, a
/// barred circle once the user turned it down, otherwise its kind's.
fn lead_icon(call: &ToolCall, settled: f32, look: ToolLook) -> Div {
    let icon = if call.rejected() {
        Icon::new(IconName::Ban)
    } else if call.failed() {
        Icon::new(IconName::CircleX).text_color(look.failed)
    } else {
        Icon::new(kind_icon(call.kind))
    };
    lead(icon, settled)
}

/// A row's leading icon, cross-fading with a spinner as `settled` goes from 0 to 1.
fn lead(icon: Icon, settled: f32) -> Div {
    let layer = |opacity: f32| div().absolute().inset_0().opacity(opacity);
    div()
        .relative()
        .flex_none()
        .size(ICON_SIZE)
        .when(settled < 1., |lead| {
            lead.child(
                layer(1. - settled).child(
                    Spinner::new()
                        .icon(IconName::LoaderCircle)
                        .with_size(ICON_SIZE),
                ),
            )
        })
        .when(settled > 0., |lead| {
            lead.child(layer(settled).child(icon.with_size(ICON_SIZE)))
        })
}

/// The chevron after a title, turning down as what it opens opens.
fn chevron(reveal: f32, look: ToolLook) -> Icon {
    Icon::new(IconName::ChevronRight)
        .with_size(CHEVRON_SIZE)
        .flex_none()
        .text_color(look.faint)
        .rotate(radians(reveal * FRAC_PI_2))
}

/// What the agent calls the call, on one line, shimmering while it runs as the waiting
/// line does.
fn title(call: &ToolCall, running: bool, mono: &SharedString, id: &ElementId) -> Div {
    let text: SharedString = call.title.replace('\n', " ").into();
    div()
        .min_w_0()
        .flex_initial()
        .truncate()
        .when(
            matches!(call.kind, ToolKind::Execute | ToolKind::Search),
            |title| title.font_family(mono.clone()).text_size(MONO_TITLE_SIZE),
        )
        .map(|title| {
            if running {
                title.child(ShimmerText::new(text).id((id.clone(), "title")))
            } else {
                title.child(text)
            }
        })
}

/// The row's end: the lines an edit adds and removes, that the call failed, was turned
/// down or never got an answer, that the user's answer holds for calls like it too, and
/// how long it ran, or has been running.
fn notes(
    call: &ToolCall,
    time: Option<String>,
    time_shown: f32,
    look: ToolLook,
    mono: &SharedString,
) -> Div {
    h_flex()
        .ml_auto()
        .pl_3()
        .flex_none()
        .gap_2()
        .text_size(NOTE_SIZE)
        .line_height(NOTE_LINE_HEIGHT)
        .text_color(look.faint)
        .when(call.added + call.removed > 0, |notes| {
            notes.child(counts(call.added, call.removed, look, mono))
        })
        .when(call.failed(), |notes| {
            notes.child(div().text_color(look.failed).child("Failed"))
        })
        .children(
            call.ask
                .as_ref()
                .and_then(|ask| match (ask.answer, ask.always) {
                    (Answer::Allowed, true) => Some("Always allowed"),
                    (Answer::Rejected, true) => Some("Always rejected"),
                    (Answer::Rejected, false) => Some("Rejected"),
                    (Answer::Cancelled, _) => Some("Cancelled"),
                    (Answer::Open, _) | (Answer::Allowed, false) => None,
                }),
        )
        .when_some(time, |notes, time| {
            notes.child(div().opacity(time_shown).child(time))
        })
}

/// "+6 −1": the lines a change adds, in green, and removes, in red.
fn counts(added: usize, removed: usize, look: ToolLook, mono: &SharedString) -> Div {
    h_flex()
        .gap_1p5()
        .font_family(mono.clone())
        .child(div().text_color(look.added).child(format!("+{added}")))
        .child(div().text_color(look.removed).child(format!("−{removed}")))
}

/// What a group's calls did, a kind at a time in the order each kind first came: "Ran 2
/// commands, read 1 file", with the calls still running as "running 1 command".
fn summary(calls: impl Iterator<Item = (ToolKind, bool)>) -> String {
    // Each kind with how many of its calls are done and how many still run.
    let mut tally: Vec<(ToolKind, usize, usize)> = Vec::new();
    for (kind, running) in calls {
        let ix = match tally.iter().position(|(seen, ..)| *seen == kind) {
            Some(ix) => ix,
            None => {
                tally.push((kind, 0, 0));
                tally.len() - 1
            }
        };
        if running {
            tally[ix].2 += 1;
        } else {
            tally[ix].1 += 1;
        }
    }
    let mut parts = Vec::new();
    for (kind, done, running) in tally {
        let (did, doing, one, many) = verbs(kind);
        let noun = |count: usize| if count == 1 { one } else { many };
        if done > 0 {
            parts.push(format!("{did} {done} {}", noun(done)));
        }
        if running > 0 {
            parts.push(format!("{doing} {running} {}", noun(running)));
        }
    }
    let text = parts.join(", ");
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How a group's header says what calls of a kind did, are doing, and what one of them
/// and several of them work on.
fn verbs(kind: ToolKind) -> (&'static str, &'static str, &'static str, &'static str) {
    match kind {
        ToolKind::Read => ("read", "reading", "file", "files"),
        ToolKind::Edit => ("edited", "editing", "file", "files"),
        ToolKind::Delete => ("deleted", "deleting", "file", "files"),
        ToolKind::Move => ("moved", "moving", "file", "files"),
        ToolKind::Search => ("ran", "running", "search", "searches"),
        ToolKind::Execute => ("ran", "running", "command", "commands"),
        // Claude's adapter sorts its subagents' tasks as thinking.
        ToolKind::Think => ("ran", "running", "task", "tasks"),
        ToolKind::Fetch => ("fetched", "fetching", "page", "pages"),
        ToolKind::SwitchMode => ("switched", "switching", "mode", "modes"),
        ToolKind::Other => ("used", "using", "tool", "tools"),
    }
}

/// How long a group took, from its first call coming in to its last one finishing, or,
/// while it runs, how long it has been running. Calls a loaded session replays have no
/// times.
fn group_time(calls: &[&ToolCall], running: bool) -> Option<String> {
    let start = calls.iter().filter_map(|call| call.started).min()?;
    if running {
        let ran = start.elapsed();
        return (ran >= COUNT_AFTER).then(|| count_text(ran));
    }
    let end = calls
        .iter()
        .filter_map(|call| Some(call.started? + call.took?))
        .max()?;
    Some(took_text(end - start))
}

fn kind_icon(kind: ToolKind) -> gpui_kit::assets::IconName {
    use gpui_kit::assets::IconName;
    match kind {
        ToolKind::Read => IconName::FileText,
        ToolKind::Edit => IconName::Pencil,
        ToolKind::Delete => IconName::Trash,
        ToolKind::Move => IconName::ArrowRightLeft,
        ToolKind::Search => IconName::Search,
        ToolKind::Execute => IconName::Terminal,
        ToolKind::Think => IconName::Lightbulb,
        ToolKind::Fetch => IconName::Globe,
        ToolKind::SwitchMode => IconName::ToggleRight,
        ToolKind::Other => IconName::Wrench,
    }
}

/// How long a finished call ran: tenths of a second under ten seconds, whole seconds
/// under a minute, then minutes and seconds.
fn took_text(took: Duration) -> String {
    let secs = took.as_secs_f32();
    // What rounds to ten tenths is ten seconds.
    if secs < 9.95 {
        format!("{secs:.1}s")
    } else {
        count_text(took.max(Duration::from_secs(10)))
    }
}

/// How long a call has run, in the whole seconds the count goes up by.
fn count_text(time: Duration) -> String {
    let secs = time.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// The boxes under an opened call, one for each piece of its content. The first that
/// isn't a diff carries the file the call works on in its header.
fn content_box(
    call: &ToolCall,
    id: &ElementId,
    folder: &Path,
    look: ToolLook,
    mono: &SharedString,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let mut location = call.locations.first().map(|location| {
        let path = shown_path(&location.path, folder);
        (path, location.line)
    });
    let mut boxes = Vec::new();
    for (n, shown) in call.content.iter().enumerate() {
        let id = ElementId::from((id.clone(), format!("box-{n}")));
        let piece = match shown {
            Shown::Code(code) => {
                let header = location.take().map(|(path, line)| {
                    let range = line_range(code).or(line.map(|line| format!("line {line}")));
                    header(
                        path,
                        range.map(|range| range.into_any_element()),
                        &id,
                        &code.copy,
                        look,
                        mono,
                    )
                });
                code_box(code, header, &id, look, mono, window, cx)
            }
            Shown::Text(view, copy) => {
                let header = location
                    .take()
                    .map(|(path, _)| header(path, None, &id, copy, look, mono));
                text_box(view, copy, header, &id, look, window, cx)
            }
            Shown::Diff(diff) => {
                let path = shown_path(&diff.path, folder);
                let counts = counts(diff.added, diff.removed, look, mono).into_any_element();
                let header = header(path, Some(counts), &id, &diff.copy, look, mono);
                diff_box(diff, Some(header), &id, look, mono, window, cx)
            }
        };
        boxes.push(piece);
    }
    // Padded rather than set off by margins, so the height it opens to takes them in.
    v_flex()
        .pl(BOX_LEFT)
        .pr(BOX_RIGHT)
        .pt_1()
        .pb_2()
        .gap_1()
        .children(boxes)
}

/// The card asking for permission to run a call, in its row's place: the kind's
/// question, a preview of what the call would do, and the agent's options in its own
/// words, the one Enter picks lit. `keyed` is whether the keys in the prompt answer it
/// (see `ChatView::card_with_keys`), which shows which keys do what.
#[allow(clippy::too_many_arguments)]
fn ask_card(
    call: &ToolCall,
    ask: &Ask,
    keyed: bool,
    key: SessionKey,
    ix: usize,
    id: &ElementId,
    folder: &Path,
    look: ToolLook,
    mono: &SharedString,
    window: &mut Window,
    cx: &mut Context<ChatView>,
) -> Div {
    let menu = MenuLook::new(cx);
    let open = ask.answer == Answer::Open;
    let options = &ask.request.options;
    let path = call
        .content
        .iter()
        .find_map(|shown| match shown {
            Shown::Diff(diff) => Some(diff.path.as_path()),
            _ => None,
        })
        .or_else(|| {
            call.locations
                .first()
                .map(|location| location.path.as_path())
        })
        .filter(|_| call.kind != ToolKind::Execute)
        .map(|path| shown_path(path, folder));
    let heading = h_flex()
        .h(CARD_ROW_HEIGHT)
        .px(CARD_ROW_PAD)
        .gap(ICON_GAP)
        .text_color(look.muted)
        .child(Icon::new(kind_icon(call.kind)).with_size(ICON_SIZE))
        .child(
            div()
                .flex_none()
                .text_size(QUESTION_SIZE)
                .line_height(ROW_LINE_HEIGHT)
                .font_weight(FontWeight::MEDIUM)
                .text_color(menu.foreground)
                .child(question(call.kind)),
        )
        .children(path.map(|path| {
            div()
                .ml_auto()
                .pl_3()
                .min_w_0()
                .truncate()
                .font_family(mono.clone())
                .text_size(NOTE_SIZE)
                .line_height(NOTE_LINE_HEIGHT)
                .text_color(look.faint)
                .child(path)
        }));

    let preview_id = ElementId::from((id.clone(), "preview"));
    let mut preview = Vec::new();
    match &ask.preview {
        Some(code) => preview.push(code_box(code, None, &preview_id, look, mono, window, cx)),
        None => {
            for (n, shown) in call.content.iter().enumerate() {
                let id = ElementId::from((preview_id.clone(), format!("box-{n}")));
                preview.push(match shown {
                    Shown::Code(code) => code_box(code, None, &id, look, mono, window, cx),
                    Shown::Text(view, copy) => text_box(view, copy, None, &id, look, window, cx),
                    Shown::Diff(diff) => diff_box(diff, None, &id, look, mono, window, cx),
                });
            }
        }
    }

    let held = ask.focus.iter().position(|focus| focus.is_focused(window));
    let lit = held.unwrap_or(ask.lit);
    let keyed = keyed || held.is_some();
    let reject = reject_option(options);
    let rows = options.iter().enumerate().map(|(n, option)| {
        let icon: gpui_kit::assets::IconName = match (option.allow, option.always) {
            (true, false) => IconName::Check.into(),
            (true, true) => gpui_kit::assets::IconName::CheckCheck,
            (false, _) => IconName::Close.into(),
        };
        let hint = keyed.then_some(()).and_then(|_| {
            if n == lit {
                Some("↵")
            } else if Some(n) == reject {
                Some("esc")
            } else {
                None
            }
        });
        let hover = menu.hover;
        let choice = option.id.clone();
        div()
            .id(ElementId::from((id.clone(), format!("option-{n}"))))
            .track_focus(&ask.focus[n])
            .tab_stop(open)
            .role(Role::Button)
            .aria_label(SharedString::from(option.name.clone()))
            .h(CARD_ROW_HEIGHT)
            .flex_none()
            .px(CARD_ROW_PAD)
            .flex()
            .items_center()
            .gap(ICON_GAP)
            .rounded(ROW_RADIUS)
            .map(|row| {
                if n == lit {
                    row.bg(menu.pressed)
                } else {
                    row.hover(move |style| style.bg(hover))
                }
            })
            .child(
                Icon::new(icon)
                    .with_size(ICON_SIZE)
                    .text_color(if n == lit {
                        menu.foreground
                    } else {
                        look.muted
                    }),
            )
            .child(div().min_w_0().truncate().child(option.name.clone()))
            .children(hint.map(|hint| {
                div()
                    .ml_auto()
                    .pl_3()
                    .flex_none()
                    .text_size(NOTE_SIZE)
                    .text_color(look.faint)
                    .child(hint)
            }))
            .when(open, |row| {
                row.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.answer(key, ix, Some(choice.clone()), window, cx)
                }))
            })
    });

    let (down, up, lit_now) = (ask.focus.clone(), ask.focus.clone(), ask.focus.clone());
    let ids: Vec<String> = options.iter().map(|option| option.id.clone()).collect();
    let no = reject.map(|n| options[n].id.clone());
    let card = menu
        .surface()
        .id(ElementId::from((id.clone(), "permission")))
        .p(CARD_PAD)
        .key_context("PermissionCard")
        .role(Role::Group)
        .aria_label(SharedString::from(question(call.kind)))
        .when(open, |card| {
            card.on_action(move |_: &SelectDown, window, cx| step_focus(&down, 1, window, cx))
                .on_action(move |_: &SelectUp, window, cx| step_focus(&up, -1, window, cx))
                .on_action(cx.listener(move |this, _: &Confirm, window, cx| {
                    let lit = lit_now.iter().position(|focus| focus.is_focused(window));
                    if let Some(lit) = lit {
                        this.answer(key, ix, Some(ids[lit].clone()), window, cx)
                    }
                }))
        })
        // Only the agent's own no: answering with none tells it the turn was stopped.
        .when_some(no.filter(|_| open), |card, no| {
            card.on_action(cx.listener(move |this, _: &Cancel, window, cx| {
                this.answer(key, ix, Some(no.clone()), window, cx)
            }))
        })
        .child(heading)
        .when(!preview.is_empty(), |card| {
            card.child(
                v_flex()
                    .px(PREVIEW_INSET)
                    .pb(PREVIEW_BELOW)
                    .gap_1()
                    .children(preview),
            )
        })
        .children(rows)
        .children(menu.edge());
    // Padded rather than set off by margins, so the height it opens to takes them in.
    div()
        .px(ROW_INSET + ROW_PAD)
        .pt(CARD_ABOVE)
        .pb(CARD_BELOW)
        .child(card)
}

/// What the card asks, by what the call would do.
fn question(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Execute => "Run this command?",
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => "Make this edit?",
        ToolKind::Read => "Read this file?",
        ToolKind::Fetch => "Fetch this page?",
        _ => "Allow this tool?",
    }
}

/// The option Esc picks: the agent's no for this call alone, or else its first no.
fn reject_option(options: &[PermissionChoice]) -> Option<usize> {
    options
        .iter()
        .position(|option| !option.allow && !option.always)
        .or_else(|| options.iter().position(|option| !option.allow))
}

/// Moves the keys `step` options along a card's, round from either end.
fn step_focus(options: &[FocusHandle], step: isize, window: &mut Window, cx: &mut App) {
    let count = options.len() as isize;
    if count == 0 {
        return;
    }
    let at = options
        .iter()
        .position(|focus| focus.is_focused(window))
        .map_or(if step > 0 { -1 } else { 0 }, |at| at as isize);
    window.focus(&options[(at + step).rem_euclid(count) as usize], cx);
}

/// A path as the box's header shows it: inside the session's folder, from there.
fn shown_path(path: &Path, folder: &Path) -> SharedString {
    path.strip_prefix(folder)
        .unwrap_or(path)
        .display()
        .to_string()
        .into()
}

/// "lines 140–155": the numbers of the first and the last line of a file read.
fn line_range(code: &Code) -> Option<String> {
    let mut numbers = code.lines.iter().filter_map(|line| line.number.clone());
    let first = numbers.next()?;
    let last = numbers.next_back().unwrap_or_else(|| first.clone());
    Some(if first == last {
        format!("line {first}")
    } else {
        format!("lines {first}–{last}")
    })
}

/// The frame every box shares: the code fill inside a hairline, rounded.
pub(super) fn frame(look: ToolLook) -> Div {
    div()
        .relative()
        .group(BOX_GROUP)
        .rounded(BOX_RADIUS)
        .bg(look.code)
        .border_1()
        .border_color(look.line)
        .overflow_hidden()
}

/// The header above a box's text: the file, what to say about it, and the copy button.
fn header(
    path: SharedString,
    end: Option<AnyElement>,
    id: &ElementId,
    copy: &SharedString,
    look: ToolLook,
    mono: &SharedString,
) -> Div {
    h_flex()
        .h(BOX_HEADER_HEIGHT)
        .pl(CODE_PAD)
        .pr(COPY_INSET)
        .gap_2()
        .border_b_1()
        .border_color(look.line)
        .text_size(NOTE_SIZE)
        .line_height(NOTE_LINE_HEIGHT)
        .text_color(look.muted)
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_family(mono.clone())
                .child(path),
        )
        .child(
            h_flex()
                .ml_auto()
                .flex_none()
                .gap(COPY_INSET)
                .children(end)
                .child(copy_button(id, copy)),
        )
}

fn copy_button(id: &ElementId, copy: &SharedString) -> Clipboard {
    Clipboard::new((id.clone(), "copy"))
        .value(copy.clone())
        .tooltip("Copy")
}

/// The copy button of a box without a header, over its top right corner while the
/// pointer is over the box.
fn floating_copy(id: &ElementId, copy: &SharedString, look: ToolLook) -> Div {
    div()
        .absolute()
        .top(COPY_INSET)
        .right(COPY_INSET)
        .rounded(ROW_RADIUS)
        .bg(look.card)
        .border_1()
        .border_color(look.line)
        .opacity(0.)
        .group_hover(BOX_GROUP, |style| style.opacity(1.))
        .child(copy_button(id, copy))
}

/// A gradient into the box's fill over its last lines, while more of it is below.
fn fade(look: ToolLook) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(BOX_FADE)
        .bg(linear_gradient(
            180.,
            linear_color_stop(look.code.opacity(0.), 0.),
            linear_color_stop(look.code, 1.),
        ))
}

/// A command's output or a file's lines, the file's numbers in a column of their own.
fn code_box(
    code: &Code,
    header: Option<Div>,
    id: &ElementId,
    look: ToolLook,
    mono: &SharedString,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let copy = header
        .is_none()
        .then(|| floating_copy(id, &code.copy, look));
    let lines = code.lines.clone();
    frame(look)
        .children(header)
        .child(lines_body(
            id,
            lines,
            code.widest,
            move |line, look| {
                h_flex()
                    .px(CODE_PAD)
                    .whitespace_nowrap()
                    .when_some(line.number.clone(), |row, number| {
                        row.child(number_cell(number, look))
                    })
                    .child(line.text.clone())
                    .into_any_element()
            },
            look,
            mono,
            window,
            cx,
        ))
        .children(copy)
}

/// A change to a file as unified diff lines: added ones green, removed ones red, each
/// with its number, and the unchanged runs between changes folded.
fn diff_box(
    diff: &Diff,
    header: Option<Div>,
    id: &ElementId,
    look: ToolLook,
    mono: &SharedString,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let copy = header
        .is_none()
        .then(|| floating_copy(id, &diff.copy, look));
    let numbered = diff.numbered;
    frame(look)
        .children(header)
        .child(lines_body(
            id,
            diff.lines.clone(),
            diff.widest,
            move |line, look| diff_line(line, numbered, look),
            look,
            mono,
            window,
            cx,
        ))
        .children(copy)
}

/// A line of a diff: an added one green, a removed one red, each with its number when
/// the diff has them, or a run of unchanged lines folded into one.
pub(super) fn diff_line(line: &DiffLine, numbered: bool, look: ToolLook) -> AnyElement {
    match line {
        DiffLine::Line {
            change,
            number,
            text,
        } => {
            let (bg, sign, sign_color) = match change {
                ChangeTag::Insert => (Some(look.added_bg), "+", look.added),
                ChangeTag::Delete => (Some(look.removed_bg), "−", look.removed),
                ChangeTag::Equal => (None, "", look.muted),
            };
            h_flex()
                .px(CODE_PAD)
                .whitespace_nowrap()
                .when_some(bg, |row, bg| row.bg(bg))
                .when(numbered, |row| row.child(number_cell(number.clone(), look)))
                .child(
                    div()
                        .w(SIGN_WIDTH)
                        .flex_none()
                        .text_color(sign_color)
                        .child(sign),
                )
                .child(text.clone())
                .into_any_element()
        }
        DiffLine::Folded(count) => h_flex()
            .px(CODE_PAD)
            .whitespace_nowrap()
            .bg(look.hover)
            .text_color(look.muted)
            .child(folded_text(*count))
            .into_any_element(),
    }
}

fn number_cell(number: SharedString, look: ToolLook) -> Div {
    div()
        .w(NUMBER_WIDTH)
        .flex_none()
        .mr_2()
        .text_right()
        .text_color(look.muted)
        .child(number)
}

/// Lines of code in a box twelve lines high at most, drawn only as they scroll into view,
/// as wide as the widest of them. A box that scrolls keeps the wheel from the transcript.
#[allow(clippy::too_many_arguments)]
fn lines_body<T: 'static>(
    id: &ElementId,
    lines: Rc<[T]>,
    widest: usize,
    line: impl Fn(&T, ToolLook) -> AnyElement + 'static,
    look: ToolLook,
    mono: &SharedString,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let scroll = window
        .use_keyed_state((id.clone(), "scroll"), cx, |_, _| {
            UniformListScrollHandle::new()
        })
        .read(cx)
        .clone();
    let count = lines.len();
    let scrolls = count > BOX_LINES;
    let at_end = scroll.is_scrolled_to_end().unwrap_or(true);
    let list = uniform_list((id.clone(), "lines"), count, move |range, _, _| {
        range.map(|ix| line(&lines[ix], look)).collect::<Vec<_>>()
    })
    .with_width_from_item(Some(widest))
    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
    .track_scroll(&scroll)
    .h(CODE_LINE_HEIGHT * count.min(BOX_LINES) as f32)
    .when(scrolls, |list| {
        list.on_scroll_wheel(|_, _, cx| cx.stop_propagation())
    });
    div()
        .relative()
        .py_2()
        .font_family(mono.clone())
        .text_size(CODE_SIZE)
        .line_height(CODE_LINE_HEIGHT)
        .text_color(look.code_text)
        .child(list)
        .when(scrolls && !at_end, |body| body.child(fade(look)))
}

/// Text the agent sent as markdown, such as a fetched page or a task's answer, in the
/// body's face rather than the monospace one.
fn text_box(
    view: &Entity<gpui_kit::component::text::TextViewState>,
    copy: &SharedString,
    header: Option<Div>,
    id: &ElementId,
    look: ToolLook,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let scroll = window
        .use_keyed_state((id.clone(), "scroll"), cx, |_, _| ScrollHandle::new())
        .read(cx)
        .clone();
    let max = scroll.max_offset().y;
    let scrolls = max > px(0.);
    let at_end = -scroll.offset().y >= max - px(1.);
    let copy = header.is_none().then(|| floating_copy(id, copy, look));
    frame(look)
        .children(header)
        .child(
            div()
                .id((id.clone(), "text"))
                .max_h(BOX_MAX_HEIGHT)
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .when(scrolls, |text| {
                    text.on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                })
                .px_3()
                .py(CODE_PAD)
                .text_size(BOX_TEXT_SIZE)
                .line_height(BOX_TEXT_LINE_HEIGHT)
                .text_color(look.code_text)
                .child(TextView::new(view).selectable(true)),
        )
        .when(scrolls && !at_end, |body| body.child(fade(look)))
        .children(copy)
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{count_text, reject_option, summary, took_text};
    use acp_core::{PermissionChoice, ToolKind};
    use std::time::Duration;

    #[test]
    fn esc_says_no_to_this_call_alone() {
        let choice = |id: &str, allow, always| PermissionChoice {
            id: id.into(),
            name: id.into(),
            allow,
            always,
        };
        let options = [
            choice("allow", true, false),
            choice("never", false, true),
            choice("reject", false, false),
        ];
        assert_eq!(reject_option(&options), Some(2));
        assert_eq!(reject_option(&options[..2]), Some(1));
        assert_eq!(reject_option(&options[..1]), None);
    }

    #[test]
    fn a_group_says_what_its_calls_did_kind_by_kind() {
        let calls = [
            (ToolKind::Execute, false),
            (ToolKind::Read, false),
            (ToolKind::Execute, false),
        ];
        assert_eq!(summary(calls.into_iter()), "Ran 2 commands, read 1 file");
        let running = [(ToolKind::Edit, false), (ToolKind::Execute, true)];
        assert_eq!(
            summary(running.into_iter()),
            "Edited 1 file, running 1 command"
        );
        let searches = [(ToolKind::Search, false), (ToolKind::Search, true)];
        assert_eq!(
            summary(searches.into_iter()),
            "Ran 1 search, running 1 search"
        );
    }

    #[test]
    fn times_read_as_the_design_has_them() {
        assert_eq!(took_text(Duration::from_millis(420)), "0.4s");
        assert_eq!(took_text(Duration::from_millis(8_440)), "8.4s");
        assert_eq!(took_text(Duration::from_millis(9_940)), "9.9s");
        assert_eq!(took_text(Duration::from_millis(9_960)), "10s");
        assert_eq!(took_text(Duration::from_millis(12_700)), "12s");
        assert_eq!(took_text(Duration::from_secs(72)), "1m 12s");
        assert_eq!(count_text(Duration::from_millis(3_900)), "3s");
        assert_eq!(count_text(Duration::from_secs(65)), "1m 05s");
    }
}
