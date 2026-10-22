//! A tool call's row in the transcript: its kind's icon, its title and how it stands, and
//! once opened, what it produced or the change it makes, in a box under the title.

use std::cell::Cell;
use std::f32::consts::FRAC_PI_2;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use acp_core::{SessionKey, ToolKind, ToolStatus};
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
use crate::session::{Code, Diff, DiffLine, Row, Shown, ToolCall, row_id};

/// A row is as tall as the menus' rows and sits a little in from the transcript's sides,
/// its padding putting the icon in line with the agent's text.
const ROW_HEIGHT: Pixels = px(28.);
const ROW_INSET: Pixels = px(10.);
const ROW_PAD: Pixels = px(6.);
const ROW_RADIUS: Pixels = px(6.);
/// Between a row and the one before or after it.
const ROW_GAP: Pixels = px(4.);
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
const CODE_SIZE: Pixels = px(12.);
const CODE_LINE_HEIGHT: Pixels = px(18.);
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
/// How long a running call goes before its row counts the seconds.
const COUNT_AFTER: Duration = Duration::from_secs(1);
/// How often the count goes up.
const COUNT_TICK: Duration = Duration::from_secs(1);
/// The row's hover group, which brightens its text and icons with its background.
const ROW_GROUP: &str = "tool-call-row";
/// The box's hover group, which shows its copy button.
const BOX_GROUP: &str = "tool-call-box";

/// The colors of the tool calls' rows and boxes, quieter than the agent's text.
#[derive(Clone, Copy)]
struct ToolLook {
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
    added: Hsla,
    added_bg: Hsla,
    removed: Hsla,
    removed_bg: Hsla,
    /// The box's fill, its edge and its text.
    code: Hsla,
    line: Hsla,
    code_text: Hsla,
    /// Behind the copy button where it floats over the box's text.
    card: Hsla,
}

impl ToolLook {
    fn new(cx: &App) -> Self {
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
    /// The tool call at `ix` of the active session's transcript: its row, and the box with
    /// what it shows, which opens and closes with the theme's control spring.
    pub(crate) fn render_tool_call(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = ToolLook::new(cx);
        let motion = cx.theme().motion_tokens();
        let (control, fast) = (motion.spring_control, motion.duration_fast);
        let mono = cx.theme().mono_font_family.clone();
        let session = &self.sessions[self.active];
        let key = session.key;
        let Some(Row::Tool(call)) = session.rows.get(ix) else {
            return div().into_any_element();
        };
        let id = row_id("tool", key, ix);
        // Claude's adapter keeps a call pending until it finishes, sending in_progress only
        // with progress beats on long ones, so any unfinished call in a live turn runs.
        let running = session.is_live() && !call.status.finished();
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
        let time = call
            .took
            .map(took_text)
            .or_else(|| counting.filter(|d| *d >= COUNT_AFTER).map(count_text));
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
            .when(expandable, |line| {
                line.child(
                    Icon::new(IconName::ChevronRight)
                        .with_size(CHEVRON_SIZE)
                        .flex_none()
                        .text_color(look.faint)
                        .rotate(radians(reveal * FRAC_PI_2)),
                )
            })
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

        // The box's height as it last laid out, which it opens and closes to. Measured
        // here rather than by the library's reveal, which lays a box out at no height the
        // frame it first draws, so an open call scrolling into view would jump.
        let height = window
            .use_keyed_state((id.clone(), "height"), cx, |_, _| Rc::new(Cell::new(None)))
            .read(cx)
            .clone();
        // The box is only laid out while open or moving: closed, it stays out of the frame.
        let content = (reveal > 0. || call.open && expandable).then(|| {
            let folder = session.folder().unwrap_or(&self.cwd).to_path_buf();
            let measured = height.clone();
            let shown = content_box(call, &id, &folder, look, &mono, window, cx)
                .on_prepaint(move |bounds, _, _| measured.set(Some(bounds.size.height)));
            div()
                .overflow_hidden()
                .opacity(reveal)
                .when(reveal < 1., |clip| {
                    clip.h(height.get().unwrap_or_default() * reveal)
                })
                .child(shown)
        });
        let element = v_flex()
            .w_full()
            .py(ROW_GAP)
            .child(div().px(ROW_INSET).child(row))
            .children(content)
            .into_any_element();
        if counting.is_some() {
            self.keep_tool_clock(cx);
        }
        element
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
                        && session.rows.iter().any(
                            |row| matches!(row, Row::Tool(call) if call.running_for().is_some()),
                        );
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

/// The icon at the row's start: a spinner while the call runs, a cross once it failed,
/// otherwise its kind's.
fn lead_icon(call: &ToolCall, settled: f32, look: ToolLook) -> Div {
    let icon = match call.status {
        ToolStatus::Failed => Icon::new(IconName::CircleX).text_color(look.failed),
        _ => Icon::new(kind_icon(call.kind)),
    };
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

/// The row's end: the lines an edit adds and removes, that the call failed, and how long
/// it ran, or has been running.
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
        .when(call.status == ToolStatus::Failed, |notes| {
            notes.child(div().text_color(look.failed).child("Failed"))
        })
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
    if secs < 10. {
        format!("{secs:.1}s")
    } else {
        count_text(took)
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
                diff_box(diff, header, &id, look, mono, window, cx)
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
fn frame(look: ToolLook) -> Div {
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
    header: Div,
    id: &ElementId,
    look: ToolLook,
    mono: &SharedString,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    frame(look).child(header).child(lines_body(
        id,
        diff.lines.clone(),
        diff.widest,
        |line, look| match line {
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
                    .child(number_cell(number.clone(), look))
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
                .child(format!("⋯ {count} unchanged lines"))
                .into_any_element(),
        },
        look,
        mono,
        window,
        cx,
    ))
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
    use super::{count_text, took_text};
    use std::time::Duration;

    #[test]
    fn times_read_as_the_design_has_them() {
        assert_eq!(took_text(Duration::from_millis(420)), "0.4s");
        assert_eq!(took_text(Duration::from_millis(8_440)), "8.4s");
        assert_eq!(took_text(Duration::from_millis(12_700)), "12s");
        assert_eq!(took_text(Duration::from_secs(72)), "1m 12s");
        assert_eq!(count_text(Duration::from_millis(3_900)), "3s");
        assert_eq!(count_text(Duration::from_secs(65)), "1m 05s");
    }
}
