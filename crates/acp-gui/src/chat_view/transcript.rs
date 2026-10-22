//! The rows of the chat's transcript: the user's prompts, the agent's text and thoughts,
//! its tool calls with their permission requests (drawn in `tool_call`), notices and
//! errors.

use std::time::Duration;

use gpui_kit::base::{Easing, SelectableText};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::text::TextView;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::session::{Row, Session, row_id};

/// How long a live turn waits on the agent before the waiting line shows, so a reply that
/// follows at once never flashes it.
const WAIT_DELAY: Duration = Duration::from_millis(250);

impl ChatView {
    /// A row of the active session's transcript. In a live turn a new row comes in as
    /// the menus do, rising the theme's short distance as it fades in, and the last row
    /// carries the waiting line while the agent has sent nothing new.
    pub(crate) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = self.row_content(ix, window, cx);
        let motion = cx.theme().motion_tokens();
        let enter = Enter {
            duration: motion.duration_normal,
            easing: motion.easing_enter.clone(),
            still: cx.reduce_motion(),
        };
        let travel = motion.distance_short.to_pixels(window.rem_size());
        let muted = cx.theme().muted_foreground;
        let session = &mut self.sessions[self.active];
        let row = if session.waiting() && ix + 1 == session.rows.len() {
            div()
                .flex()
                .flex_col()
                .child(row)
                .child(waiting_line(session, ix, muted, &enter, window))
                .into_any_element()
        } else {
            row
        };
        let Some(arrived) = session.entering.get(&ix) else {
            return row;
        };
        let Some(delta) = enter.at(arrived.elapsed(), window) else {
            session.entering.remove(&ix);
            return row;
        };
        // Moved by its offset, which leaves the list's measure of the row alone.
        div()
            .w_full()
            .relative()
            .top(travel * (1. - delta))
            .opacity(delta)
            .child(row)
            .into_any_element()
    }

    /// The row at `ix` and the one after it are both tool calls, which sit close together
    /// rather than a paragraph apart.
    pub(crate) fn joins_next(&self, ix: usize) -> bool {
        let rows = &self.sessions[self.active].rows;
        matches!(
            (rows.get(ix), rows.get(ix + 1)),
            (Some(Row::Tool(_)), Some(Row::Tool(_)))
        )
    }

    fn row_content(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, bubble, danger, selection) = (
            theme.muted_foreground,
            theme.secondary,
            theme.danger,
            theme.selection,
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
                        .child(
                            SelectableText::new(row_id("user", key, ix), text.clone())
                                .selection_color(selection),
                        ),
                )
                .into_any_element(),
            Row::Agent(view) => row
                .child(TextView::new(view).selectable(true))
                .into_any_element(),
            Row::Thought(view) => row
                .text_sm()
                .italic()
                .text_color(muted)
                .child(TextView::new(view).selectable(true))
                .into_any_element(),
            Row::Tool(_) => self.render_tool_call(ix, window, cx),
            Row::Notice(text) => row
                .text_sm()
                .text_color(muted)
                .child(
                    SelectableText::new(row_id("notice", key, ix), text.clone())
                        .selection_color(selection),
                )
                .into_any_element(),
            Row::Error(text) => row
                .text_sm()
                .text_color(danger)
                .child(
                    SelectableText::new(row_id("error", key, ix), text.clone())
                        .selection_color(selection),
                )
                .into_any_element(),
        }
    }
}

/// "Thinking…" under the last row, with the component library's shimmer, while a live
/// turn waits on the agent. Its space is there from the start, so the row that takes its
/// place doesn't move the transcript; it fades in once the rows have kept still for
/// `WAIT_DELAY`.
fn waiting_line(
    session: &Session,
    ix: usize,
    muted: Hsla,
    enter: &Enter,
    window: &mut Window,
) -> impl IntoElement {
    let waited = session.changed.elapsed().checked_sub(WAIT_DELAY);
    let fade = match waited {
        Some(waited) => enter.at(waited, window).unwrap_or(1.),
        // Drawn again each frame until the delay is over, Reduce motion or not, so the
        // line appears then: nothing else may draw the window.
        None => {
            window.request_animation_frame();
            0.
        }
    };
    div()
        .px_4()
        .py_1p5()
        .text_sm()
        .text_color(muted)
        .opacity(fade)
        .child(ShimmerText::new("Thinking…").id(row_id("waiting", session.key, ix)))
}

/// How a row or the waiting line comes in: the theme's normal duration and entering curve,
/// or at once while macOS's Reduce motion is on.
struct Enter {
    duration: Duration,
    easing: Easing,
    still: bool,
}

impl Enter {
    /// How far in it is `elapsed` after it started, asking for the next frame while it
    /// moves; `None` once it is in.
    fn at(&self, elapsed: Duration, window: &mut Window) -> Option<f32> {
        let progress = elapsed.as_secs_f32() / self.duration.as_secs_f32();
        if self.still || progress >= 1. {
            return None;
        }
        window.request_animation_frame();
        Some(self.easing.sample(progress))
    }
}
