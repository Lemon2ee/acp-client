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
    /// once they are back at it.
    list: ListState,
}

impl TranscriptScroll {
    pub(crate) fn new() -> Self {
        let list = ListState::new(0, ListAlignment::Top, OVERDRAW);
        list.set_follow_mode(FollowMode::Tail);
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
        let state = session.scroller.read(cx).list.clone();
        let scrolled_up = !state.is_following_tail();
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
                                .on_click(cx.listener({
                                    let state = state.clone();
                                    move |_, _: &ClickEvent, _, cx| {
                                        // Follows again from the end, as the next layout puts it.
                                        state.set_follow_mode(FollowMode::Tail);
                                        cx.notify();
                                    }
                                }))
                                .disabled(!scrolled_up),
                        ),
                )
            })
    }
}
