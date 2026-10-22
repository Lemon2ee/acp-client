//! The transcript's list. gpui-component's `MessageScroller` keeps the newest text in view
//! by putting the list's end at the bottom on every layout, so each line a live reply adds
//! shoves the transcript up at once. This list glides there instead, over the theme's
//! normal duration. The rest is as `MessageScroller` has it: scrolling up stops the
//! following and scrolling back to the end starts it again, the rows are spaced the same,
//! and a button brings the newest text back.

use std::time::{Duration, Instant};

use gpui_kit::base::{ScrollBounce, Transition, transition};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;

/// How far past the viewport the list lays rows out, as in `MessageScroller`.
const OVERDRAW: Pixels = px(400.);
/// How long the button back to the newest text takes to come and go, as in
/// `MessageScroller`.
const JUMP_BUTTON_FADE: Duration = Duration::from_millis(200);
/// Closer to the end than this counts as there.
const SETTLED: Pixels = px(0.5);
/// One frame at 60Hz. The first step of a glide after the view has kept still covers
/// this long, not all the time since the last frame.
const LONGEST_STEP: Duration = Duration::from_micros(16_667);

/// Where a session's transcript is scrolled, kept per session so each keeps its place
/// while another is on screen.
pub(crate) struct TranscriptScroll {
    /// In the list's normal follow mode: its tail mode would put the end in view itself
    /// on each layout.
    list: ListState,
    /// The newest text is kept in view: until the user scrolls away from the end, and
    /// again once they are back at it.
    following: bool,
    /// Where `follow` left the top of the view, to tell the user's scrolls from its own.
    placed: Option<ListOffset>,
    /// When `follow` last ran.
    ran: Instant,
    /// The viewport's height then, as the list last laid out.
    height: Pixels,
    /// Rows changed since then. The list measures them as it lays out, after `follow`, so
    /// the end they move is only known a frame later.
    unmeasured: bool,
    /// The rows last changed in a live turn, so the view glides to their end. A loaded
    /// session's replay goes straight there, as it always shows at once.
    glide: bool,
}

impl TranscriptScroll {
    pub(crate) fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, OVERDRAW),
            following: true,
            placed: None,
            ran: Instant::now(),
            height: px(0.),
            unmeasured: false,
            glide: false,
        }
    }

    /// A row was added at the end; `live` is whether a live turn added it.
    pub(crate) fn push(&mut self, live: bool) {
        let count = self.list.item_count();
        self.list.splice(count..count, 1);
        // The row that was last now has the gap before the next one under it.
        if let Some(last) = count.checked_sub(1) {
            self.list.remeasure_items(last..count);
        }
        self.unmeasured = true;
        self.glide = live;
    }

    /// The row at `ix` changed and may have changed height.
    pub(crate) fn remeasure(&mut self, ix: usize, live: bool) {
        self.list.remeasure_items(ix..ix + 1);
        self.unmeasured = true;
        self.glide = live;
    }

    /// The session comes on screen. One that follows its newest text shows the end of it
    /// at once, where rows that came while it was away would otherwise glide in.
    pub(crate) fn catch_up(&mut self) {
        if self.following {
            self.jump_to_end();
        }
    }

    fn jump_to_end(&mut self) {
        // Settles on the end as the list next lays out, however much it has grown.
        self.list.scroll_to_end();
        self.following = true;
        self.placed = None;
    }

    /// Run each frame before the list lays out: notices the user's scrolls, and while the
    /// view follows, moves it toward the end the last layout measured. Each step takes
    /// the share of the way left that a curve settling over `duration` covers in a
    /// frame, so text that keeps coming is chased smoothly. More than a screen away, after
    /// the window resizes, or with Reduce motion on, the view goes straight there.
    fn follow(&mut self, duration: Duration, reduce_motion: bool, window: &mut Window) {
        let now = Instant::now();
        let since = now.duration_since(self.ran).min(LONGEST_STEP);
        self.ran = now;
        if std::mem::take(&mut self.unmeasured) && self.following {
            window.request_animation_frame();
        }
        let height = self.list.viewport_bounds().size.height;
        let resized = height != self.height;
        self.height = height;
        if height <= px(0.) {
            // Not laid out yet: a session shown with its history starts at the end of it.
            self.list.scroll_to_end();
            return;
        }
        let top = -self.list.scroll_px_offset_for_scrollbar().y;
        let end = self.list.max_offset_for_scrollbar().y;
        if let Some(placed) = self.placed
            && moved(placed, self.list.logical_scroll_top())
        {
            // The user scrolled, or rows shrank under the view; the latter leaves it at
            // the end, which the next check picks up again.
            self.following = false;
        }
        if top >= end - SETTLED {
            self.following = true;
        }
        let gap = end - top;
        if self.following && gap > SETTLED {
            let left = if !self.glide || reduce_motion || resized || gap > height {
                px(0.)
            } else {
                // An exponential ease-out: within 2% of the end after `duration`.
                gap * (-4. * since.as_secs_f32() / duration.as_secs_f32()).exp()
            };
            let next = if left > SETTLED { end - left } else { end };
            self.list.set_offset_from_scrollbar(point(px(0.), -next));
            if next < end {
                window.request_animation_frame();
            }
        }
        self.placed = Some(self.list.logical_scroll_top());
    }
}

/// Whether the top of the view is somewhere else than `before`. Compared as rows and
/// offsets into them, so a row above the view changing height doesn't count.
fn moved(before: ListOffset, now: ListOffset) -> bool {
    before.item_ix != now.item_ix || (before.offset_in_item - now.offset_in_item).abs() > SETTLED
}

impl ChatView {
    /// The active session's transcript, scrolled to follow its newest text.
    pub(crate) fn render_transcript(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let duration = theme.motion_tokens().duration_normal;
        let radius = theme.radius_full();
        let colors = theme.semantic_tokens().colors;
        let reduce_motion = cx.reduce_motion();
        let session = &self.sessions[self.active];
        // Keyed per session, so a switch never reuses another session's element state.
        let id = ElementId::Name(format!("messages-{}", session.key.0).into());
        let scroller = session.scroller.clone();
        let (state, scrolled_up) = scroller.update(cx, |scroller, _| {
            scroller.follow(duration, reduce_motion, window);
            (scroller.list.clone(), !scroller.following)
        });
        let jump_shown = transition(
            (id.clone(), "jump-button-shown"),
            if scrolled_up { 1. } else { 0. },
            Transition::new(JUMP_BUTTON_FADE),
            window,
            cx,
        );

        // The list's own padding would lie past what the wheel and the scrollbar reach,
        // as they measure the rows alone, so the first and last rows carry it.
        let count = state.item_count();
        let render_row =
            cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx));
        let rows = list(state.clone(), move |ix, window, cx| {
            div()
                .w_full()
                .min_w_0()
                .px_3()
                .when(ix == 0, |row| row.pt_2())
                .map(|row| {
                    if ix + 1 < count {
                        row.pb_8()
                    } else {
                        row.pb_2()
                    }
                })
                .child(render_row(ix, window, cx))
                .into_any_element()
        })
        .size_full()
        .min_h_0();

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
                    .child(ScrollBounce::new((id.clone(), "bounce"), &state, rows))
                    .vertical_scrollbar(&state),
            )
            .when(jump_shown > 0., |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(rems(0.5 + jump_shown * 0.5))
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
                                    scroller.update(cx, |scroller, _| scroller.jump_to_end());
                                    cx.notify();
                                }))
                                .disabled(!scrolled_up),
                        ),
                )
            })
    }
}
