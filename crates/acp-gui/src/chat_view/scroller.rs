//! The transcript's list. It keeps the newest text in view as gpui-component's
//! `MessageScroller` does, with the list's tail mode: each layout puts the end at the
//! bottom, so a line a live reply adds moves the transcript up in the same frame it shows.
//! Gliding there instead trailed each new line, which read as a jolt per line. Scrolling up
//! stops the following, scrolling back to the end starts it again, and a button brings the
//! newest text back.
//!
//! Its own list rather than `MessageScroller` for the padding: the wheel and the scrollbar
//! reach only the rows' own height, so padding on the list left them 16px short of the end,
//! where tail mode never starts following again. The first and last rows carry it here.
//!
//! The list sits in gpui-kit's `ScrollBounce`, which stretches its edge as macOS does, with a
//! limit on how far a finger can push past either end (see `EDGE_PUSH`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::base::{ScrollBounce, ScrollBounceMotion, Transition, transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::debug::{since_last_frame, switches};

/// How far past the viewport the list lays rows out, as in `MessageScroller`.
const OVERDRAW: Pixels = px(400.);
/// How long the button back to the newest text takes to come and go, as in
/// `MessageScroller`.
const JUMP_BUTTON_FADE: Duration = Duration::from_millis(200);
/// How far above the transcript's bottom that button starts as it comes in, and how far
/// it rises from there, in rems, as in `MessageScroller`.
const JUMP_BUTTON_FROM: f32 = 0.5;
const JUMP_BUTTON_RISE: f32 = 0.5;
/// How far a finger may push the transcript past either end before the rest is dropped,
/// in the wheel's pixels. gpui-kit's bounce (gpui-base 0.8.0) adds up every pixel pushed
/// past the end but draws the edge at most 36px out, 33px of it by 800px, so a finger
/// that turned back had to undo all of it before anything moved: after a long push, the
/// whole trackpad. At 600px the edge is about 30px out, and turning back moves it at once.
const EDGE_PUSH: f32 = 600.;

/// Where a session's transcript is scrolled, kept per session so each keeps its place
/// while another is on screen.
pub(crate) struct TranscriptScroll {
    /// In tail mode: it follows the end until the user scrolls away from it, and again
    /// once they are back at it. In normal mode with `ACP_GUI_DEBUG=no-follow`.
    list: ListState,
    /// How far a finger has pushed it past an end, read and written by the wheel.
    edge_push: Rc<Cell<EdgePush>>,
}

impl TranscriptScroll {
    pub(crate) fn new() -> Self {
        let list = ListState::new(0, ListAlignment::Top, OVERDRAW);
        if switches().follow {
            list.set_follow_mode(FollowMode::Tail);
        }
        Self {
            list,
            edge_push: Rc::default(),
        }
    }

    /// A row was added at the end.
    pub(crate) fn push(&mut self) {
        let count = self.list.item_count();
        self.list.splice(count..count, 1);
        // The row that was last now has the gap before the next one under it.
        if let Some(last) = count.checked_sub(1) {
            self.list.remeasure_items(last..count);
        }
    }

    /// The row at `ix` changed and may have changed height.
    pub(crate) fn remeasure(&mut self, ix: usize) {
        self.list.remeasure_items(ix..ix + 1);
    }

    /// Whether the newest text is out of view, which brings up the button back to it.
    pub(crate) fn scrolled_up(&self) -> bool {
        if switches().follow {
            !self.list.is_following_tail()
        } else {
            self.list.is_scrolled_to_end() == Some(false)
        }
    }

    /// Back to the newest text, and following it again where it follows at all.
    fn jump_to_end(&self) {
        if switches().follow {
            self.list.set_follow_mode(FollowMode::Tail);
        } else {
            self.list.scroll_to_end();
        }
    }

    /// One line of `ACP_GUI_DEBUG=log` for a frame, from the list as it last laid out:
    /// the time since the last frame, the last row's height, where the view's top is
    /// against the furthest it can go, and how much of the streaming row's text shows.
    fn log_frame(&self, (shown, arrived): (usize, usize)) {
        let gap = since_last_frame().map_or(0., |gap| gap.as_secs_f32() * 1000.);
        let count = self.list.item_count();
        let last = count
            .checked_sub(1)
            .and_then(|last| self.list.bounds_for_item(last))
            .map_or(0., |bounds| bounds.size.height.as_f32());
        let top = -self.list.scroll_px_offset_for_scrollbar().y.as_f32();
        let end = self.list.max_offset_for_scrollbar().y.as_f32();
        let follow = if self.list.is_following_tail() {
            "following"
        } else {
            "still"
        };
        eprintln!(
            "frame +{gap:5.1}ms  rows {count:3}  last {last:7.1}px  top {top:8.1} / end \
             {end:8.1}  {follow:9}  text {shown}/{arrived}"
        );
    }
}

/// Which end of the transcript the view is at.
#[derive(Clone, Copy, PartialEq, Debug)]
enum End {
    Top,
    Bottom,
}

impl End {
    /// The end the list is at, if it can scroll at all.
    fn of(list: &ListState) -> Option<Self> {
        let max = list.max_offset_for_scrollbar().y;
        if max <= px(0.) {
            // The bounce leaves a transcript shorter than its view alone.
            return None;
        }
        // Tail mode's anchor reads past the end until the next layout places it, so the
        // bottom is anything from `max` on.
        let top = -list.scroll_px_offset_for_scrollbar().y;
        if top <= px(0.5) {
            Some(End::Top)
        } else if top >= max - px(0.5) {
            Some(End::Bottom)
        } else {
            None
        }
    }

    /// Whether a wheel packet of `dy` pushes past this end; positive `dy` scrolls toward
    /// the top.
    fn outward(self, dy: f32) -> bool {
        match self {
            End::Top => dy > 0.,
            End::Bottom => dy < 0.,
        }
    }
}

/// The travel a finger has pushed past an end of the transcript, counted as gpui-kit's
/// bounce counts it, so that `EDGE_PUSH` can stop it there.
#[derive(Clone, Copy, Default)]
struct EdgePush {
    /// The end, and how far past it the travel not yet taken back goes.
    past: Option<(End, f32)>,
    /// Whether a finger is on the trackpad. Only a held edge adds up travel: momentum
    /// and a phaseless wheel throw it instead, once.
    held: bool,
    /// When the finger last let go, after which the edge returns.
    let_go: Option<Instant>,
}

impl EdgePush {
    /// Counts one wheel packet against the list at `end`, and says whether it goes on to
    /// the bounce.
    fn packet(&mut self, dy: f32, phase: TouchPhase, end: Option<End>, now: Instant) -> bool {
        match phase {
            TouchPhase::Started => {
                // A finger that catches the edge on its way back takes it where it is.
                let left = self.let_go.map_or(0., |at| returning(now - at));
                self.past = match (self.past, end) {
                    (Some((past, travel)), Some(end)) if past == end => Some((end, travel * left)),
                    _ => None,
                };
                self.held = true;
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.held = false;
                self.let_go = Some(now);
            }
            TouchPhase::Moved if self.held && dy != 0. => {
                let Some(end) = end else {
                    self.past = None;
                    return true;
                };
                let travel = match self.past {
                    Some((past, travel)) if past == end => travel,
                    _ => 0.,
                };
                if end.outward(dy) {
                    if travel >= EDGE_PUSH {
                        return false;
                    }
                    self.past = Some((end, travel + dy.abs()));
                } else {
                    self.past = Some((end, (travel - dy.abs()).max(0.)));
                }
            }
            TouchPhase::Moved => {}
        }
        true
    }
}

/// How much of a let-go edge is still out `since` it was let go: the bounce brings it back
/// critically damped at the pace its motion sets. Read off the travel rather than the
/// stretch drawn from it, it errs high, so the limit can only come early.
fn returning(since: Duration) -> f32 {
    let response = ScrollBounceMotion::default().response().as_secs_f32();
    if response <= 0. {
        return 0.;
    }
    let t = std::f32::consts::TAU / response * since.as_secs_f32();
    (1. + t) * (-t).exp()
}

/// Holds back the wheel packets that would push the transcript past `EDGE_PUSH`. Drawn
/// before the bounce, so its listener sees each packet first; it takes no space.
fn edge_push_limit(list: ListState, edge_push: Rc<Cell<EdgePush>>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, cx| {
            if !ScrollBounce::enabled_by_default() || cx.reduce_motion() {
                return;
            }
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let ScrollDelta::Pixels(delta) = event.delta else {
                    return;
                };
                // A sideways packet doesn't move the bounce.
                let dy = if delta.x.abs() > delta.y.abs() {
                    0.
                } else {
                    delta.y.as_f32()
                };
                let mut push = edge_push.get();
                let goes_on = push.packet(dy, event.touch_phase, End::of(&list), Instant::now());
                edge_push.set(push);
                if !goes_on {
                    cx.stop_propagation();
                }
            });
        },
    )
    .absolute()
    .size_full()
}

impl ChatView {
    /// The active session's transcript, scrolled to follow its newest text.
    pub(crate) fn render_transcript(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let radius = theme.radius_full();
        let colors = theme.semantic_tokens().colors;
        let session = &self.sessions[self.active];
        // Keyed per session, so a switch never reuses another session's element state.
        let id = ElementId::Name(format!("messages-{}", session.key.0).into());
        let scroller = session.scroller.read(cx);
        if switches().log {
            scroller.log_frame(session.paced_text());
        }
        let state = scroller.list.clone();
        let scrolled_up = scroller.scrolled_up();
        let edge_push = scroller.edge_push.clone();
        let scroller = session.scroller.clone();
        let jump_shown = transition(
            (id.clone(), "jump-button-shown"),
            if scrolled_up { 1. } else { 0. },
            Transition::new(JUMP_BUTTON_FADE),
            window,
            cx,
        );

        // The first and last rows carry the list's padding (see the module's doc). Tool
        // calls in a row sit close together.
        let count = state.item_count();
        let render_row = cx.processor(move |this, ix: usize, window, cx| {
            let joined = this.joins_next(ix);
            div()
                .w_full()
                .min_w_0()
                .px_3()
                .when(ix == 0, |row| row.pt_2())
                .map(|row| {
                    if ix + 1 == count {
                        row.pb_2()
                    } else if joined {
                        row
                    } else {
                        row.pb_8()
                    }
                })
                .child(this.render_row(ix, window, cx))
                .into_any_element()
        });
        let rows = list(state.clone(), render_row).size_full().min_h_0();

        div()
            .id(id.clone())
            .relative()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(
                div()
                    .id((id.clone(), "viewport"))
                    // Announced as a log, whose added rows a screen reader reads out.
                    .role(Role::Log)
                    .size_full()
                    .min_h_0()
                    .min_w_0()
                    .child(edge_push_limit(state.clone(), edge_push))
                    .child(ScrollBounce::new((id.clone(), "bounce"), &state, rows))
                    .vertical_scrollbar(&state),
            )
            .when(jump_shown > 0., |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(rems(JUMP_BUTTON_FROM + jump_shown * JUMP_BUTTON_RISE))
                        .flex()
                        .justify_center()
                        .opacity(jump_shown)
                        .child(
                            Button::new((id, "jump-to-latest"))
                                .secondary()
                                .icon(IconName::ArrowDown)
                                .tooltip("Jump to latest")
                                .rounded(radius)
                                .border_1()
                                .border_color(colors.border)
                                .bg(colors.background)
                                .text_color(colors.foreground)
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    // The next layout puts the end in view.
                                    scroller.read(cx).jump_to_end();
                                    cx.notify();
                                }))
                                .disabled(!scrolled_up),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui_kit::TouchPhase;

    use super::{EDGE_PUSH, EdgePush, End};

    /// Holds the end, pushes `dy` a packet at a time for `travel`, and says how many
    /// pixels of it went on to the bounce.
    fn push(edge: &mut EdgePush, end: End, dy: f32, travel: f32, now: Instant) -> f32 {
        let mut sent = 0.;
        let mut done = 0.;
        while done < travel {
            if edge.packet(dy, TouchPhase::Moved, Some(end), now) {
                sent += dy.abs();
            }
            done += dy.abs();
        }
        sent
    }

    #[test]
    fn a_held_push_past_the_end_stops_at_the_limit_and_turns_back_at_once() {
        let now = Instant::now();
        let mut edge = EdgePush::default();
        assert!(edge.packet(0., TouchPhase::Started, Some(End::Bottom), now));
        let sent = push(&mut edge, End::Bottom, -20., 3000., now);
        assert!((EDGE_PUSH..EDGE_PUSH + 20.).contains(&sent), "{sent}");
        // Back the other way, every packet goes on and takes the push back.
        assert_eq!(push(&mut edge, End::Bottom, 20., 600., now), 600.);
        assert_eq!(edge.past, Some((End::Bottom, 0.)));
        // The top's own push is counted the other way round.
        let mut edge = EdgePush::default();
        edge.packet(0., TouchPhase::Started, Some(End::Top), now);
        assert!(push(&mut edge, End::Top, 20., 3000., now) < EDGE_PUSH + 20.);
    }

    #[test]
    fn momentum_and_text_away_from_the_ends_go_through() {
        let now = Instant::now();
        let mut edge = EdgePush::default();
        // Momentum after a let-go, or a wheel with no phases, is never held.
        edge.packet(0., TouchPhase::Ended, Some(End::Bottom), now);
        assert_eq!(push(&mut edge, End::Bottom, -20., 3000., now), 3000.);
        // Held, away from either end, nothing counts.
        edge.packet(0., TouchPhase::Started, None, now);
        for _ in 0..100 {
            assert!(edge.packet(-20., TouchPhase::Moved, None, now));
        }
        assert_eq!(edge.past, None);
    }

    #[test]
    fn catching_a_returning_edge_counts_what_is_left_of_the_push() {
        let now = Instant::now();
        let mut edge = EdgePush::default();
        edge.packet(0., TouchPhase::Started, Some(End::Bottom), now);
        push(&mut edge, End::Bottom, -20., 3000., now);
        edge.packet(0., TouchPhase::Ended, Some(End::Bottom), now);
        // Caught at once, the edge is still out: nothing more goes past.
        edge.packet(0., TouchPhase::Started, Some(End::Bottom), now);
        assert!(!edge.packet(-20., TouchPhase::Moved, Some(End::Bottom), now));
        // Caught once it is back, the whole limit is there again.
        edge.packet(0., TouchPhase::Ended, Some(End::Bottom), now);
        let later = now + Duration::from_secs(2);
        edge.packet(0., TouchPhase::Started, Some(End::Bottom), later);
        assert!(push(&mut edge, End::Bottom, -20., 3000., later) >= EDGE_PUSH);
    }
}
