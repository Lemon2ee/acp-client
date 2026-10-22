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

use std::time::Duration;

use gpui_kit::base::{ScrollBounce, Transition, transition};
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

/// Where a session's transcript is scrolled, kept per session so each keeps its place
/// while another is on screen.
pub(crate) struct TranscriptScroll {
    /// In tail mode: it follows the end until the user scrolls away from it, and again
    /// once they are back at it. In normal mode with `ACP_GUI_DEBUG=no-follow`.
    list: ListState,
}

impl TranscriptScroll {
    pub(crate) fn new() -> Self {
        let list = ListState::new(0, ListAlignment::Top, OVERDRAW);
        if switches().follow {
            list.set_follow_mode(FollowMode::Tail);
        }
        Self { list }
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
    fn scrolled_up(&self) -> bool {
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
        let scroller = session.scroller.clone();
        let jump_shown = transition(
            (id.clone(), "jump-button-shown"),
            if scrolled_up { 1. } else { 0. },
            Transition::new(JUMP_BUTTON_FADE),
            window,
            cx,
        );

        // The first and last rows carry the list's padding (see the module's doc). Tool
        // calls sit close to each other and to the agent's text around them.
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
