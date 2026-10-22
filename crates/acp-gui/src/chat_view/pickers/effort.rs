//! The effort meter: the button for how hard the model thinks, whose level a scroll over
//! it or an arrow key moves one at a time, and the slider of the levels that a press on
//! it opens above it. A change shows at once and goes to the agent once the user stops
//! (see `ChatView::hold_setting`).

mod wheel;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use acp_core::{SessionKey, Setting, SettingChoice, SettingKind, SettingValue};
use gpui_kit::base::actions::{
    Confirm, SelectDown, SelectFirst, SelectLast, SelectLeft, SelectRight, SelectUp,
};
use gpui_kit::base::{MotionStatus, Transition, transition, transition_with_status};
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::InputGroupButton;
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::{
    EFFORT_MENU_WIDTH, LEAD_GAP, LIST_ROW_HEIGHT, MENU_HEADING_HEIGHT, MENU_HEADING_SIZE,
    MENU_LINE_HEIGHT, MENU_OFFSET, MENU_TEXT_SIZE, METER_BAR_WIDTH, METER_HEIGHT, MenuLook,
    PICKER_HEIGHT, ROW_PAD, ROW_RADIUS, SURFACE_PADDING, check_column, listed_choices, rise_in,
    shown_choice,
};
use crate::session::row_id;
use crate::sound::{self, TICK_GAP};
use wheel::{Take, Wheel, natural_scrolling, toward_more};

/// The button has no caret, so it is as far in on the right as on the left.
const BUTTON_PAD: Pixels = px(8.);
/// The slider's row of marks, inside the surface's padding: 40px tall, 4.5px in from
/// either side, each level's share of it holding a group of ticks 3px in from its ends.
const TRACK_HEIGHT: Pixels = px(40.);
const TRACK_PAD: Pixels = px(4.5);
const LEVEL_PAD: Pixels = px(3.);
/// A tick: 2px wide, one for each 5px of a level's share, 16px tall, and up to 10px
/// taller under the pointer, falling off over about 10px either side of it.
const TICK_WIDTH: Pixels = px(2.);
const TICK_PITCH: f32 = 5.;
const TICK_HEIGHT: f32 = 16.;
const TICK_LIFT: f32 = 10.;
const TICK_SPREAD: f32 = 10.;
/// How long the level a scroll or a key reaches stands taller, while the pointer is off
/// the slider.
const WAKE: Duration = Duration::from_millis(140);
/// How far the meter and the slider give, and come back, when pushed past an end.
const NUDGE: f32 = 2.;
/// The level's name at the end of the slider's heading.
const VALUE_SIZE: Pixels = px(12.);
/// Above and below the line over the slider's Default row.
const DEFAULT_GAP: Pixels = px(4.);
/// What the Default row says of itself: Claude's adapter says nothing of which level its
/// "Default" is.
const DEFAULT_NOTE: &str = "model decides";
/// The button's tooltip, after gpui's usual delay.
const HINT: &str = "Scroll or click to change";
/// Groups the button with its meter's unreached bars, which brighten while it is
/// pointed at, as a sign that it does something.
const BUTTON_GROUP: &str = "effort-button";

/// A setting's levels of how hard the model thinks, lowest first, and where it stands.
struct Levels<'a> {
    levels: Vec<&'a SettingChoice>,
    /// A "default" that is a choice of its own (see `stand_in`): Claude's adapter gives
    /// one that says nothing of which level it is. The slider lists it under the levels,
    /// and the meter shows it faintly, as far as `assumed`.
    default: Option<&'a SettingChoice>,
    /// The level in use, or none while the setting is at its "default".
    at: Option<usize>,
    /// The choice the setting shows, and the setting's own name for want of one.
    shown: Option<&'a SettingChoice>,
    setting: &'a str,
}

impl<'a> Levels<'a> {
    fn new(setting: &'a Setting) -> Self {
        let (default, levels): (Vec<_>, Vec<_>) =
            listed_choices(setting).partition(|choice| choice.id == "default");
        let shown = shown_choice(setting);
        let at = shown.and_then(|shown| levels.iter().position(|level| level.id == shown.id));
        Self {
            levels,
            default: default.into_iter().next(),
            at,
            shown,
            setting: &setting.name,
        }
    }

    /// Where the "default" is taken to stand, for its faint bars and for the first step
    /// from it, which lands beside it: Medium, which Claude's adapter recommends to the
    /// clients that read its recommendations, or else the middle level.
    fn assumed(&self) -> usize {
        let middle = self.levels.len().saturating_sub(1) / 2;
        let medium = self.levels.iter().position(|level| level.id == "medium");
        medium.unwrap_or(middle)
    }

    /// The level `dir` levels on from where the setting stands, or none past an end.
    fn step(&self, dir: isize) -> Option<usize> {
        let from = self.at.unwrap_or_else(|| self.assumed());
        from.checked_add_signed(dir)
            .filter(|&to| to < self.levels.len())
    }

    /// The choice at `at`: a level, or the "default" for none.
    fn choice(&self, at: Option<usize>) -> Option<&'a SettingChoice> {
        match at {
            Some(at) => self.levels.get(at).copied(),
            None => self.default,
        }
    }

    /// What the setting is at, by name.
    fn name(&self) -> SharedString {
        let name = self.shown.map_or(self.setting, |shown| shown.name.as_str());
        SharedString::from(name.to_string())
    }
}

/// How a level's marks look.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tone {
    /// At or below the level in use.
    Reached,
    Unreached,
    /// At or below where a "default" is taken to stand.
    Assumed,
    /// About to change if the level under the pointer were picked.
    Previewed,
}

impl Tone {
    /// The marks at `level`, with the setting at `at` (none at its "default", taken to
    /// stand at `assumed`) and the pointer over `preview`.
    fn of(level: usize, at: Option<usize>, assumed: usize, preview: Option<usize>) -> Self {
        let reached = at.is_some_and(|at| level <= at);
        match preview.filter(|&preview| Some(preview) != at) {
            None if reached => Tone::Reached,
            None if at.is_none() && level <= assumed => Tone::Assumed,
            None => Tone::Unreached,
            Some(preview) => match (reached, level <= preview) {
                (true, true) => Tone::Reached,
                (false, false) => Tone::Unreached,
                _ => Tone::Previewed,
            },
        }
    }

    fn color(self, look: &MenuLook) -> Hsla {
        match self {
            Tone::Reached => look.foreground,
            Tone::Unreached => look.meter_off,
            Tone::Assumed => look.meter_assumed,
            Tone::Previewed => look.meter_previewed,
        }
    }
}

/// A change of level, which the marks between the two follow one after another.
#[derive(Clone, Copy)]
struct Sweep {
    key: SessionKey,
    from: Option<usize>,
    to: Option<usize>,
    at: Instant,
}

impl Sweep {
    /// How long the `mark`th of the `marks` marks at `level` waits before it changes:
    /// the marks between the two levels light up, or go dark, in turn, each level's in
    /// `TICK_GAP` however many it has. Only while the change plays out, `fade` long a
    /// mark: a later change, as the pointer moves, shows at once.
    fn delay(
        &self,
        key: SessionKey,
        level: usize,
        mark: usize,
        marks: usize,
        fade: Duration,
    ) -> Duration {
        let from = self.from.map_or(-1, |from| from as isize);
        let to = self.to.map_or(-1, |to| to as isize);
        let (low, high, level) = (from.min(to), from.max(to), level as isize);
        let playing = self.at.elapsed() < TICK_GAP * (high - low) as u32 + fade;
        if self.key != key || !playing || level <= low || level > high {
            return Duration::ZERO;
        }
        let rank = if to > from {
            (level - low - 1) as usize * marks + mark
        } else {
            (high - level) as usize * marks + (marks - 1 - mark)
        };
        TICK_GAP * rank as u32 / marks as u32
    }
}

/// Where a change comes from.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    Wheel,
    Keys,
    /// A press or a drag on the slider, where the pointer shows the way already.
    Pointer,
}

/// A move of the level.
#[derive(Clone, Copy)]
enum Step {
    /// One level up (1) or down (-1).
    Level(isize),
    /// To the top (1) or the bottom (-1).
    End(isize),
}

/// What the effort meter keeps between frames. Only the session on screen's shows, so
/// one does for all; the level itself is the session's.
pub(crate) struct EffortMeter {
    /// Shared with the listener that sees every wheel event in the window.
    wheel: Rc<RefCell<Wheel>>,
    sweep: Option<Sweep>,
    /// Pushed past an end: which way, up (1) or down (-1), and when.
    nudge: Option<(isize, Instant)>,
    /// The slider is open.
    open: bool,
    /// The slider's, which holds the keys while it is open, and the Default row's after
    /// it in the Tab order.
    focus: FocusHandle,
    default_focus: FocusHandle,
    /// Where the pointer is along the slider's marks, from the track's left edge, while
    /// it is over them or dragging them.
    pointer: Option<Pixels>,
    /// The left button went down on the slider's marks and hasn't come up.
    dragging: bool,
    /// The level a scroll or a key last reached while the pointer was off the open
    /// slider, and when: it stands taller for a moment.
    wake: Option<(usize, Instant)>,
}

impl EffortMeter {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            wheel: Rc::default(),
            sweep: None,
            nudge: None,
            open: false,
            focus: cx.focus_handle().tab_stop(true),
            default_focus: cx.focus_handle().tab_stop(true),
            pointer: None,
            dragging: false,
            wake: None,
        }
    }
}

/// How many ticks a level's share of the slider holds, at one for each `TICK_PITCH`: six
/// for Claude's five levels, eight for Codex's four.
fn ticks_per_level(levels: usize) -> usize {
    let share = track_width() / levels.max(1) as f32;
    let room = share - LEVEL_PAD * 2. - TICK_WIDTH;
    (room.as_f32() / TICK_PITCH).floor().max(0.) as usize + 1
}

/// How wide the slider's marks run, inside the surface's and the track's padding.
fn track_width() -> Pixels {
    EFFORT_MENU_WIDTH - SURFACE_PADDING * 2. - TRACK_PAD * 2.
}

/// The level whose share of the track `x` is in, from the track's left edge: anything
/// before the first is the first, and past the last the last.
fn level_at(x: Pixels, levels: usize) -> usize {
    let share = track_width() / levels.max(1) as f32;
    let level = ((x - TRACK_PAD) / share).floor().max(0.) as usize;
    level.min(levels.saturating_sub(1))
}

/// The middle of a level's share of the track, from its left edge.
fn level_center(level: usize, levels: usize) -> Pixels {
    let share = track_width() / levels.max(1) as f32;
    TRACK_PAD + share * (level as f32 + 0.5)
}

/// The middle of the `mark`th of a level's `marks` ticks, from the track's left edge.
/// The ticks spread over the level's share, the first and last at its padding.
fn tick_center(level: usize, mark: usize, marks: usize, levels: usize) -> Pixels {
    let share = track_width() / levels.max(1) as f32;
    let spread = if marks > 1 {
        (share - LEVEL_PAD * 2. - TICK_WIDTH) * (mark as f32 / (marks - 1) as f32)
    } else {
        px(0.)
    };
    TRACK_PAD + share * level as f32 + LEVEL_PAD + spread + TICK_WIDTH / 2.
}

/// How far the meter and the slider stand off their place after a push past an end:
/// out by `NUDGE` over the first 40% of the theme's normal duration and back over the
/// rest, each part on the entering curve. GPUI has no transforms, so it moves by an
/// offset.
fn nudged(nudge: Option<(isize, Instant)>, window: &Window, cx: &App) -> Pixels {
    let Some((dir, at)) = nudge else {
        return px(0.);
    };
    let motion = cx.theme().motion_tokens();
    let t = at.elapsed().as_secs_f32() / motion.duration_normal.as_secs_f32();
    if t >= 1. {
        return px(0.);
    }
    window.request_animation_frame();
    let out = if t < 0.4 {
        motion.easing_enter.sample(t / 0.4)
    } else {
        1. - motion.easing_enter.sample((t - 0.4) / 0.6)
    };
    px(NUDGE * out * dir as f32)
}

/// How wide `text` is in the buttons' 13px medium text, to ease the button from one
/// name's width to another's.
fn text_width(text: &SharedString, window: &Window, cx: &App) -> Pixels {
    let mut run_font = font(cx.theme().font_family.clone());
    run_font.weight = FontWeight::MEDIUM;
    let run = TextRun {
        len: text.len(),
        font: run_font,
        color: black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.clone(), MENU_TEXT_SIZE, &[run], None);
    line.width
}

/// Has the window's wheel events, wherever they land, reach the meter before anything
/// takes them, so it can tell scrolling that started elsewhere. It draws nothing, and
/// goes on the button, which shows whenever the slider does.
fn watch_wheel(wheel: &Rc<RefCell<Wheel>>) -> impl IntoElement {
    let wheel = wheel.clone();
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, _| {
                if phase == DispatchPhase::Capture {
                    wheel.borrow_mut().see(event.touch_phase, Instant::now());
                }
            });
        },
    )
    .absolute()
}

/// What a wheel event over the button or the slider does: once it has gone far enough,
/// a level up or down, unless it belongs to scrolling elsewhere. Taken, it goes no
/// further.
fn take_wheel(
    view: WeakEntity<ChatView>,
    wheel: Rc<RefCell<Wheel>>,
    key: SessionKey,
) -> impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static {
    move |event, _, cx| {
        let more = toward_more(&event.delta, natural_scrolling());
        let take = wheel
            .borrow_mut()
            .take(event.touch_phase, &event.delta, more, Instant::now());
        match take {
            Take::Pass => return,
            Take::Hold => {}
            Take::Step(dir) => {
                let moved = view.update(cx, |this, cx| {
                    this.step_effort(key, Step::Level(dir), Source::Wheel, cx)
                });
                if !matches!(moved, Ok(true)) {
                    wheel.borrow_mut().stop();
                }
            }
        }
        cx.stop_propagation();
    }
}

/// Has `element` take the meter's keys (see `main`'s key bindings): Up or Right a level
/// up, Down or Left a level down, Home and End to either end.
fn take_keys<E: InteractiveElement>(element: E, view: &WeakEntity<ChatView>, key: SessionKey) -> E {
    let on = |step: Step| {
        let view = view.clone();
        move |cx: &mut App| {
            let _ = view.update(cx, |this, cx| this.step_effort(key, step, Source::Keys, cx));
        }
    };
    let (up, right) = (on(Step::Level(1)), on(Step::Level(1)));
    let (down, left) = (on(Step::Level(-1)), on(Step::Level(-1)));
    let (first, last) = (on(Step::End(-1)), on(Step::End(1)));
    element
        .on_action(move |_: &SelectUp, _, cx| up(cx))
        .on_action(move |_: &SelectRight, _, cx| right(cx))
        .on_action(move |_: &SelectDown, _, cx| down(cx))
        .on_action(move |_: &SelectLeft, _, cx| left(cx))
        .on_action(move |_: &SelectFirst, _, cx| first(cx))
        .on_action(move |_: &SelectLast, _, cx| last(cx))
}

/// The slider, as `ChatView` has the meter when the window is drawn.
struct Slider {
    key: SessionKey,
    setting: Setting,
    pointer: Option<Pixels>,
    wake: Option<(usize, Instant)>,
    sweep: Option<Sweep>,
    nudge: Option<(isize, Instant)>,
    focus: FocusHandle,
    default_focus: FocusHandle,
    wheel: Rc<RefCell<Wheel>>,
    view: WeakEntity<ChatView>,
}

impl Slider {
    /// The slider: the setting's name over its levels as a row of thin ticks, the level
    /// under the pointer named at the heading's end, and a "default" of its own on a row
    /// below. It holds the keys while open: the meter's keys move the level, and Enter,
    /// Space or Esc close it.
    fn render(
        &self,
        popover: &Entity<PopoverState>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnimationElement<Stateful<Div>> {
        let look = MenuLook::new(cx);
        let levels = Levels::new(&self.setting);
        let count = levels.levels.len();
        let preview = self
            .pointer
            .filter(|_| count > 0)
            .map(|x| level_at(x, count));
        let previewing = preview.filter(|&level| Some(level) != levels.at);
        let value = match previewing {
            Some(level) => SharedString::from(levels.levels[level].name.clone()),
            None => levels.name(),
        };
        let heading = div()
            .h(MENU_HEADING_HEIGHT)
            .flex_none()
            .px(ROW_PAD)
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .child(
                div()
                    .text_size(MENU_HEADING_SIZE)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(look.muted)
                    .child(SharedString::from(self.setting.name.clone())),
            )
            .child(
                div()
                    .text_size(VALUE_SIZE)
                    .font_weight(FontWeight::MEDIUM)
                    .when(previewing.is_some(), |value| value.text_color(look.muted))
                    .child(value),
            );
        let track = self.track(&levels, preview, &look, window, cx);
        let default = levels
            .default
            .map(|default| self.default_row(default, levels.at.is_none(), &look));
        let (view, key, dismiss) = (self.view.clone(), self.key, popover.clone());
        let (increment, decrement) = (view.clone(), view.clone());
        let surface = look
            .surface()
            .id("effort-slider")
            .track_focus(&self.focus)
            .key_context("EffortMeter")
            .role(Role::Slider)
            .aria_label(SharedString::from(self.setting.name.clone()))
            .aria_orientation(Orientation::Horizontal)
            .aria_min_numeric_value(0.)
            .aria_max_numeric_value(count.saturating_sub(1) as f64)
            .aria_numeric_value_step(1.)
            .aria_numeric_value(levels.at.unwrap_or_else(|| levels.assumed()) as f64)
            .aria_value(levels.name())
            .on_a11y_action(AccessibleAction::Increment, move |_, _, cx| {
                let _ = increment.update(cx, |this, cx| {
                    this.step_effort(key, Step::Level(1), Source::Keys, cx)
                });
            })
            .on_a11y_action(AccessibleAction::Decrement, move |_, _, cx| {
                let _ = decrement.update(cx, |this, cx| {
                    this.step_effort(key, Step::Level(-1), Source::Keys, cx)
                });
            })
            .on_action(move |_: &Confirm, window, cx| {
                dismiss.update(cx, |state, cx| state.dismiss(window, cx));
            })
            .on_scroll_wheel(take_wheel(view.clone(), self.wheel.clone(), key))
            .w(EFFORT_MENU_WIDTH)
            .p(SURFACE_PADDING)
            .flex()
            .flex_col()
            .whitespace_nowrap()
            .child(heading)
            .child(track)
            .children(default)
            .children(look.edge());
        let surface = take_keys(surface, &view, key);
        rise_in(surface, "effort-slider-in", window, cx)
    }

    /// The levels as groups of ticks, lit up to the level in use. The ticks stand taller
    /// under the pointer, or for a moment at a level a scroll or a key reaches, and a
    /// change lights them up, or puts them out, one after another.
    fn track(
        &self,
        levels: &Levels,
        preview: Option<usize>,
        look: &MenuLook,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        let motion = cx.theme().motion_tokens();
        let fade = motion.duration_fast;
        let quick = Transition::new(fade).easing(motion.easing_enter.clone());
        let count = levels.levels.len();
        let marks = ticks_per_level(count);
        let assumed = levels.assumed();
        let waking = self.wake.filter(|(_, at)| at.elapsed() < WAKE);
        if waking.is_some() {
            window.request_animation_frame();
        }
        let crest = self
            .pointer
            .or_else(|| waking.map(|(level, _)| level_center(level, count)));
        let lift = |x: Pixels| {
            crest.map_or(0., |crest| {
                let off = (x - crest).as_f32();
                TICK_LIFT * (-(off * off) / (2. * TICK_SPREAD * TICK_SPREAD)).exp()
            })
        };
        let mut shares = Vec::with_capacity(count);
        for level in 0..count {
            let color = Tone::of(level, levels.at, assumed, preview).color(look);
            let mut ticks = Vec::with_capacity(marks);
            for mark in 0..marks {
                let id = row_id("effort-tick", self.key, level * marks + mark);
                let delay = self.sweep.map_or(Duration::ZERO, |sweep| {
                    sweep.delay(self.key, level, mark, marks, fade)
                });
                let color = transition(
                    (id.clone(), "tone"),
                    color,
                    quick.clone().delay(delay),
                    window,
                    cx,
                );
                let height = transition(
                    (id, "height"),
                    TICK_HEIGHT + lift(tick_center(level, mark, marks, count)),
                    quick.clone(),
                    window,
                    cx,
                );
                ticks.push(
                    div()
                        .flex_none()
                        .w(TICK_WIDTH)
                        .h(px(height))
                        .rounded(px(1.))
                        .bg(color),
                );
            }
            shares.push(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .px(LEVEL_PAD)
                    .flex()
                    .items_center()
                    .justify_between()
                    .children(ticks),
            );
        }
        div()
            .relative()
            .left(nudged(self.nudge, window, cx))
            .flex_none()
            .h(TRACK_HEIGHT)
            .px(TRACK_PAD)
            .flex()
            .children(shares)
            .child(self.track_pointer())
    }

    /// Takes the pointer over the track: a press picks the level under it, a drag
    /// carries the level along, past either end to that end, and moving over it shows
    /// what a press would pick.
    fn track_pointer(&self) -> impl IntoElement {
        let (view, key) = (self.view.clone(), self.key);
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
                let along = move |position: Point<Pixels>| position.x - bounds.left();
                window.on_mouse_event({
                    let (view, hitbox) = (view.clone(), hitbox.clone());
                    move |event: &MouseDownEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble
                            && event.button == MouseButton::Left
                            && hitbox.is_hovered(window)
                        {
                            let x = along(event.position);
                            let _ = view.update(cx, |this, cx| this.press_effort(key, x, cx));
                        }
                    }
                });
                window.on_mouse_event({
                    let view = view.clone();
                    move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble {
                            let (x, over) = (along(event.position), hitbox.is_hovered(window));
                            let held = event.dragging();
                            let _ = view.update(cx, |this, cx| {
                                this.move_over_effort(key, x, over, held, cx)
                            });
                        }
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        let _ = view.update(cx, |this, _| this.effort.dragging = false);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    /// The row for a "default" of the setting's own, below the levels, with a check while
    /// the setting is at it. A press or Enter picks it and leaves the slider open.
    fn default_row(&self, default: &SettingChoice, current: bool, look: &MenuLook) -> Div {
        let (view, key, hover) = (self.view.clone(), self.key, look.hover);
        let pick = move |cx: &mut App| {
            let _ = view.update(cx, |this, cx| this.pick_effort_default(key, cx));
        };
        let confirm = pick.clone();
        div()
            .mt(DEFAULT_GAP)
            .pt(DEFAULT_GAP)
            .border_t_1()
            .border_color(look.line)
            .child(
                div()
                    .id("effort-default")
                    .track_focus(&self.default_focus)
                    .role(Role::RadioButton)
                    .aria_label(SharedString::from(default.name.clone()))
                    .aria_toggled(if current {
                        Toggled::True
                    } else {
                        Toggled::False
                    })
                    .h(LIST_ROW_HEIGHT)
                    .pl(ROW_PAD)
                    .pr(px(4.))
                    .flex()
                    .items_center()
                    .gap(LEAD_GAP)
                    .rounded(ROW_RADIUS)
                    .cursor_pointer()
                    .hover(move |row| row.bg(hover))
                    .focus_visible(move |row| row.bg(hover))
                    .child(check_column(current))
                    .child(SharedString::from(default.name.clone()))
                    .child(
                        div()
                            .ml_auto()
                            .text_size(VALUE_SIZE)
                            .text_color(look.muted)
                            .child(DEFAULT_NOTE),
                    )
                    .on_click(move |_, _, cx| pick(cx))
                    .on_action(move |_: &Confirm, _, cx| confirm(cx)),
            )
    }
}

impl ChatView {
    /// The effort meter's button, and the slider it opens upward. The button shows the
    /// level as a meter and by name, with no caret; it is only as wide as the name, and
    /// eases to a new name's width so the mode button beside it slides rather than
    /// jumps. A scroll over it or the meter's keys on it move the level.
    pub(crate) fn render_effort_picker(
        &self,
        key: SessionKey,
        setting: &Setting,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let button = self.effort_button(key, setting, &view, window, cx);
        let slider = Slider {
            key,
            setting: setting.clone(),
            pointer: self.effort.pointer,
            wake: self.effort.wake,
            sweep: self.effort.sweep,
            nudge: self.effort.nudge,
            focus: self.effort.focus.clone(),
            default_focus: self.effort.default_focus.clone(),
            wheel: self.effort.wheel.clone(),
            view: view.clone(),
        };
        Popover::new("effort-slider")
            .appearance(false)
            .anchor(Anchor::BottomLeft)
            .offset(MENU_OFFSET)
            .track_focus(&self.effort.focus)
            .on_open_change(move |open, _, cx| {
                let _ = view.update(cx, |this, cx| this.effort_slider_shown(key, *open, cx));
            })
            .trigger(button.cursor_pointer())
            .content(move |_, window, cx| {
                let popover = cx.entity();
                slider.render(&popover, window, cx)
            })
            .into_any_element()
    }

    fn effort_button(
        &self,
        key: SessionKey,
        setting: &Setting,
        view: &WeakEntity<ChatView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> InputGroupButton {
        let look = MenuLook::new(cx);
        let motion = cx.theme().motion_tokens();
        let (fade, ease, enter) = (
            motion.duration_fast,
            motion.duration_normal,
            motion.easing_enter.clone(),
        );
        let levels = Levels::new(setting);
        let count = levels.levels.len();
        let assumed = levels.assumed();
        let mut bars = Vec::with_capacity(count);
        for bar in 0..count {
            // The last bar is full height and each one before it 2px shorter.
            let height = (METER_HEIGHT - px(2.) * (count - 1 - bar) as f32).max(px(2.));
            let tone = Tone::of(bar, levels.at, assumed, None);
            let delay = self
                .effort
                .sweep
                .map_or(Duration::ZERO, |sweep| sweep.delay(key, bar, 0, 1, fade));
            let color = transition(
                (row_id("effort-bar", key, bar), "tone"),
                tone.color(&look),
                Transition::new(fade).easing(enter.clone()).delay(delay),
                window,
                cx,
            );
            let off_hover = look.meter_off_hover;
            bars.push(
                div()
                    .w(METER_BAR_WIDTH)
                    .h(height)
                    .rounded(px(1.))
                    .bg(color)
                    .when(tone == Tone::Unreached, |bar| {
                        bar.group_hover(BUTTON_GROUP, move |bar| bar.bg(off_hover))
                    }),
            );
        }
        let meter = (count >= 2).then(|| {
            div()
                .flex_none()
                .relative()
                .left(nudged(self.effort.nudge, window, cx))
                .h(METER_HEIGHT)
                .flex()
                .items_end()
                .gap(METER_BAR_WIDTH)
                .children(bars)
        });
        let name = levels.name();
        let width = transition_with_status(
            (row_id("effort-name", key, 0), "width"),
            text_width(&name, window, cx).as_f32(),
            Transition::new(ease).easing(enter),
            window,
            cx,
        );
        let easing = matches!(width.status, MotionStatus::Delayed | MotionStatus::Running);
        let label = format!("{}: {name}", setting.name);
        let button = InputGroupButton::new("effort-picker")
            .custom(look.button(cx))
            .small()
            .accessibility_label(label)
            .h(PICKER_HEIGHT)
            .px(BUTTON_PAD)
            .rounded(ROW_RADIUS)
            .group(BUTTON_GROUP)
            .key_context("EffortMeter")
            .children(meter)
            .child(
                // The button's own 14px text is set on the row the name sits in.
                div()
                    .flex_none()
                    .text_size(MENU_TEXT_SIZE)
                    .line_height(MENU_LINE_HEIGHT)
                    .whitespace_nowrap()
                    // While it eases, as wide as it has got; then as wide as the name.
                    .when(easing, |name| name.w(px(width.value)).overflow_hidden())
                    .child(name),
            )
            .child(watch_wheel(&self.effort.wheel))
            .on_scroll_wheel(take_wheel(view.clone(), self.effort.wheel.clone(), key))
            // Over the open slider it would cover the heading.
            .when(!self.effort.open, |button| button.tooltip(HINT));
        take_keys(button, view, key)
    }

    /// The session's effort setting, as it shows.
    fn effort_setting(&self, key: SessionKey) -> Option<Setting> {
        let session = self.session(key)?;
        self.shown_settings(session)
            .into_iter()
            .find(|setting| setting.kind == SettingKind::ThoughtLevel)
    }

    /// Moves the session's effort a level, or to an end. Past an end it knocks and the
    /// meter gives a little. Whether it moved.
    fn step_effort(
        &mut self,
        key: SessionKey,
        step: Step,
        source: Source,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(setting) = self.effort_setting(key) else {
            return false;
        };
        let levels = Levels::new(&setting);
        let (to, dir) = match step {
            Step::Level(dir) => (levels.step(dir), dir),
            Step::End(dir) => {
                let end = if dir > 0 {
                    levels.levels.len().checked_sub(1)
                } else {
                    Some(0)
                };
                (end.filter(|_| !levels.levels.is_empty()), dir)
            }
        };
        if to.is_some() && self.set_effort(key, &setting, to, source, cx) {
            return true;
        }
        sound::knock();
        if !cx.reduce_motion() {
            self.effort.nudge = Some((dir, Instant::now()));
            cx.notify();
        }
        false
    }

    /// Sets the session's effort to the level `to`, or its "default" for none, unless it
    /// is there already. It shows at once, ticks once for each level it passes, and goes
    /// to the agent once the user stops.
    fn set_effort(
        &mut self,
        key: SessionKey,
        setting: &Setting,
        to: Option<usize>,
        source: Source,
        cx: &mut Context<Self>,
    ) -> bool {
        let levels = Levels::new(setting);
        let Some(choice) = levels.choice(to).filter(|_| to != levels.at) else {
            return false;
        };
        let value = SettingValue::Choice(choice.id.clone());
        self.hold_setting(key, setting.id.clone(), value, cx);
        let now = Instant::now();
        self.effort.sweep = Some(Sweep {
            key,
            from: levels.at,
            to,
            at: now,
        });
        let effort = &mut self.effort;
        if source != Source::Pointer
            && effort.open
            && effort.pointer.is_none()
            && !cx.reduce_motion()
        {
            effort.wake = to.map(|to| (to, now));
        }
        tick_through(setting, &levels, to, cx);
        true
    }

    /// Picks the setting's "default" of its own, from the slider's row for it.
    fn pick_effort_default(&mut self, key: SessionKey, cx: &mut Context<Self>) {
        if let Some(setting) = self.effort_setting(key) {
            self.set_effort(key, &setting, None, Source::Pointer, cx);
        }
    }

    /// Picks the level under the pointer, `x` along the slider's track.
    fn pick_effort_at(&mut self, key: SessionKey, x: Pixels, cx: &mut Context<Self>) {
        let Some(setting) = self.effort_setting(key) else {
            return;
        };
        let count = Levels::new(&setting).levels.len();
        if count > 0 {
            self.set_effort(key, &setting, Some(level_at(x, count)), Source::Pointer, cx);
        }
    }

    /// The left button went down `x` along the slider's track.
    fn press_effort(&mut self, key: SessionKey, x: Pixels, cx: &mut Context<Self>) {
        self.effort.dragging = true;
        self.effort.pointer = Some(x);
        self.effort.wake = None;
        self.pick_effort_at(key, x, cx);
        cx.notify();
    }

    /// The pointer moved, `x` along the slider's track, `over` it or not, with the left
    /// button `held` down or not. While dragging the level follows it anywhere.
    fn move_over_effort(
        &mut self,
        key: SessionKey,
        x: Pixels,
        over: bool,
        held: bool,
        cx: &mut Context<Self>,
    ) {
        if self.effort.dragging && held {
            self.effort.pointer = Some(x);
            self.pick_effort_at(key, x, cx);
            cx.notify();
            return;
        }
        // The button came up somewhere this listener didn't hear.
        self.effort.dragging = false;
        let pointer = over.then_some(x);
        if pointer != self.effort.pointer {
            self.effort.pointer = pointer;
            if pointer.is_some() {
                self.effort.wake = None;
            }
            cx.notify();
        }
    }

    /// The slider opened or closed. Closing it sends the level it was left at, without
    /// waiting for the pause.
    fn effort_slider_shown(&mut self, key: SessionKey, open: bool, cx: &mut Context<Self>) {
        let effort = &mut self.effort;
        effort.open = open;
        effort.pointer = None;
        effort.dragging = false;
        effort.wake = None;
        if !open {
            self.send_held(key, cx);
        }
        cx.notify();
    }
}

/// Ticks once for each level a change from where `levels` stands to `to` passes, in time
/// with the marks lighting up, higher for a higher level. A change to or from a
/// "default" passes none and ticks once, as does any change with reduced motion, where
/// the marks change at once.
fn tick_through(setting: &Setting, levels: &Levels, to: Option<usize>, cx: &mut Context<ChatView>) {
    let pitch = |choice: &SettingChoice| {
        listed_choices(setting)
            .position(|listed| listed.id == choice.id)
            .unwrap_or(0)
    };
    let passed: Vec<usize> = match (levels.at, to) {
        (Some(from), Some(to)) if !cx.reduce_motion() => {
            let passed: Vec<usize> = if to > from {
                (from + 1..=to).collect()
            } else {
                (to..from).rev().collect()
            };
            passed
                .into_iter()
                .map(|level| pitch(levels.levels[level]))
                .collect()
        }
        _ => levels.choice(to).map(pitch).into_iter().collect(),
    };
    let mut passed = passed.into_iter();
    if let Some(first) = passed.next() {
        sound::tick(first);
    }
    let rest: Vec<usize> = passed.collect();
    if !rest.is_empty() {
        cx.spawn(async move |_, cx| {
            for pitch in rest {
                cx.background_executor().timer(TICK_GAP).await;
                sound::tick(pitch);
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use acp_core::{SessionKey, Setting, SettingChoice, SettingKind, SettingValue};
    use gpui_kit::px;

    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{Levels, Sweep, Tone, level_at, level_center, ticks_per_level};

    fn effort(ids: &[&str], value: &str) -> Setting {
        let choice = |id: &&str| SettingChoice {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
        };
        Setting {
            id: "effort".into(),
            name: "Effort".into(),
            kind: SettingKind::ThoughtLevel,
            value: SettingValue::Choice(value.into()),
            choices: ids.iter().map(choice).collect(),
        }
    }

    const CLAUDE: [&str; 6] = ["default", "low", "medium", "high", "xhigh", "max"];

    #[test]
    fn claudes_default_stands_apart_and_steps_from_medium() {
        let setting = effort(&CLAUDE, "default");
        let levels = Levels::new(&setting);
        assert_eq!(levels.levels.len(), 5);
        assert_eq!(levels.at, None);
        assert_eq!(levels.assumed(), 1);
        assert_eq!(levels.step(1), Some(2));
        assert_eq!(levels.step(-1), Some(0));
    }

    #[test]
    fn steps_stop_at_either_end() {
        let setting = effort(&CLAUDE, "max");
        let levels = Levels::new(&setting);
        assert_eq!(levels.at, Some(4));
        assert_eq!(levels.step(1), None);
        let setting = effort(&["low", "medium", "high", "xhigh"], "low");
        assert_eq!(Levels::new(&setting).step(-1), None);
    }

    #[test]
    fn a_preview_marks_what_would_change() {
        // At High, pointing at Low: Medium and High would go dark.
        let tones: Vec<Tone> = (0..5)
            .map(|level| Tone::of(level, Some(2), 1, Some(0)))
            .collect();
        use Tone::*;
        assert_eq!(tones, [Reached, Previewed, Previewed, Unreached, Unreached]);
        // At the default, the levels up to Medium are faint.
        let tones: Vec<Tone> = (0..5).map(|level| Tone::of(level, None, 1, None)).collect();
        assert_eq!(tones, [Assumed, Assumed, Unreached, Unreached, Unreached]);
    }

    #[test]
    fn the_ticks_fill_each_levels_share() {
        assert_eq!(ticks_per_level(5), 6);
        assert_eq!(ticks_per_level(4), 8);
        assert_eq!(level_at(px(0.), 5), 0);
        assert_eq!(level_at(px(400.), 5), 4);
        assert_eq!(level_at(level_center(3, 5), 5), 3);
    }

    #[test]
    fn a_change_lights_its_marks_in_turn() {
        let (key, at) = (SessionKey(1), Instant::now());
        let sweep = Sweep {
            key,
            from: Some(0),
            to: Some(2),
            at,
        };
        let fade = Duration::from_millis(120);
        assert_eq!(sweep.delay(key, 0, 0, 2, fade), Duration::ZERO);
        assert_eq!(sweep.delay(key, 1, 1, 2, fade), Duration::from_millis(14));
        assert_eq!(sweep.delay(key, 2, 1, 2, fade), Duration::from_millis(42));
        // Going down, from the top mark.
        let sweep = Sweep {
            key,
            from: Some(2),
            to: Some(0),
            at,
        };
        assert_eq!(sweep.delay(key, 2, 1, 2, fade), Duration::ZERO);
        assert_eq!(sweep.delay(key, 1, 0, 2, fade), Duration::from_millis(42));
        // Another session's marks don't wait.
        assert_eq!(sweep.delay(SessionKey(2), 1, 0, 2, fade), Duration::ZERO);
    }
}
