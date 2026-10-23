//! A card dragged off the Stuff board into the chat. Let go over the prompt box, its text
//! goes into the prompt after what is there, not sent. Let go over the transcript, it is
//! sent at once, unless the agent is still replying, when it goes into the prompt as
//! well. Anywhere else, or on Esc, it flies back. The card stays on the board either way.
//!
//! gpui's drag view is gone once the pointer lets go, so the card's way into the prompt,
//! onto its sent row or back to the board is drawn by `ChatView` over the whole window.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use acp_core::SessionKey;
use gpui_kit::base::{Easing, Interpolate as _, Transition, transition};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::MenuLook;
use crate::chat_view::stuff::{
    CARD_PAD, CARD_RADIUS, CardLook, LINE_HEIGHT, SLOT_EDGE_ALPHA, TEXT_SIZE, dashed_edge, measure,
    mix_shadows,
};
use crate::config::StuffNote;

/// A card let go of fades from 120ms, over 160ms, as it reaches its place.
const FADE_AFTER: Duration = Duration::from_millis(120);
const FADE: Duration = Duration::from_millis(160);
/// Its text goes into the prompt as it reaches the prompt's first line.
const INTO_PROMPT_AFTER: Duration = Duration::from_millis(160);
/// From 300ms the card on the board shows again, and the prompt's ring lets go over the
/// slow duration.
const SETTLE_AFTER: Duration = Duration::from_millis(300);
/// Where in the prompt zone a card going in heads for: the box is 12px in, and its first
/// line 8px further right and 4px further down.
const INTO_PROMPT: Point<Pixels> = point(px(20.), px(16.));
/// The ring a zone lights while a card is over it: 1.5px inside the edge at the text's
/// color, 55%, and a 4px halo at 8%. The prompt box's own 1px border becomes it.
const RING_WIDTH: Pixels = px(1.5);
const RING_ALPHA: f32 = 0.55;
const HALO_WIDTH: Pixels = px(4.);
const HALO_ALPHA: f32 = 0.08;
/// The veil over the transcript: 3% of the text's color, 12px in from the chat's sides
/// and 4px below its top, ending where the prompt zone starts, as round as the box.
const VEIL_ALPHA: f32 = 0.03;
const VEIL_INSET: Pixels = px(12.);
const VEIL_TOP: Pixels = px(4.);
const VEIL_RADIUS: Pixels = px(12.);
/// What letting go would do, on a pill: 24px tall on the menus' surface, 8px above the
/// prompt box or 12px above the veil's foot. It comes up 4px as it fades in.
const PILL_HEIGHT: Pixels = px(24.);
const PILL_PAD_X: Pixels = px(10.);
const PILL_TEXT_SIZE: Pixels = px(12.);
const PILL_GAP: Pixels = px(8.);
const VEIL_PILL_BOTTOM: Pixels = px(12.);
/// The prompt zone's padding around the box (`p_3`), which the pill sits in.
const PROMPT_ZONE_PAD: Pixels = px(12.);

/// What a card on the board carries when it is picked up.
pub(super) struct StuffDrag {
    id: u64,
}

impl StuffDrag {
    pub(super) fn new(id: u64) -> Self {
        Self { id }
    }
}

/// The card being dragged, its way to where it was let go, and where the zones it can go
/// to were last laid out.
#[derive(Default)]
pub(super) struct DragState {
    held: Option<Held>,
    dropped: Option<Dropped>,
    /// The zone under the pointer.
    over: Option<Zone>,
    /// The last zone the pointer was over, which the prompt's pill keeps saying as it
    /// fades.
    last: Option<Zone>,
    zones: Rc<Zones>,
}

/// A card in hand.
struct Held {
    note: StuffNote,
    /// Where on the card the pointer holds it.
    grab: Point<Pixels>,
    size: Size<Pixels>,
}

/// A card let go of, on its way to `to` from where it was let go, in the window.
struct Dropped {
    note: StuffNote,
    to: DropTo,
    from: Bounds<Pixels>,
    started: Instant,
}

#[derive(Clone, Copy)]
enum DropTo {
    /// Into the prompt box's first line.
    Prompt,
    /// Onto the top right corner of the row it was sent as.
    Sent { key: SessionKey, row: usize },
    /// Back to its place on the board.
    Back,
}

/// Where letting go puts a card.
#[derive(Clone, Copy, PartialEq)]
enum Zone {
    /// Into the prompt. `busy` when the pointer is over the transcript, but the agent is
    /// replying and takes no prompt.
    Prompt { busy: bool },
    /// Sent from the transcript.
    History,
}

/// The window's parts as last laid out, in the window.
#[derive(Default)]
struct Zones {
    root: Cell<Bounds<Pixels>>,
    chat: Cell<Bounds<Pixels>>,
    /// The prompt box and the 12px around it.
    prompt: Cell<Bounds<Pixels>>,
    /// The bubble of the row a card was sent as.
    sent: Cell<Option<Bounds<Pixels>>>,
}

/// One frame of a card let go of: how far along each part is, eased, 0 to 1.
struct DropFrame {
    /// The card's way to its place.
    moved: f32,
    /// The card fading out into the prompt or its sent row.
    faded: f32,
    /// The sent row filling in under the card.
    filled: f32,
    /// How much of the card on the board is still just its place.
    hollow: f32,
    /// Past 300ms: the card on the board coming back, and the prompt's ring letting go.
    settling: bool,
}

impl DragState {
    /// A card is in hand: picked up, and the pointer not let go yet.
    fn dragging(&self, cx: &App) -> bool {
        self.held.is_some() && cx.has_active_drag()
    }

    /// The zone a card in hand is over.
    fn over(&self, cx: &App) -> Option<Zone> {
        self.over.filter(|_| self.dragging(cx))
    }

    /// How much of the card with `id` on the board is just its place, while it is in
    /// hand or on its way somewhere.
    pub(super) fn hollow(&self, id: u64, cx: &App) -> f32 {
        if self.dragging(cx) && self.held.as_ref().is_some_and(|held| held.note.id == id) {
            return 1.;
        }
        self.dropped
            .as_ref()
            .filter(|dropped| dropped.note.id == id)
            .and_then(|_| self.drop_frame(cx))
            .map_or(0., |frame| frame.hollow)
    }

    fn drop_frame(&self, cx: &App) -> Option<DropFrame> {
        let dropped = self.dropped.as_ref()?;
        let motion = cx.theme().motion_tokens();
        let (normal, slow) = (motion.duration_normal, motion.duration_slow);
        let total = match dropped.to {
            DropTo::Back => slow,
            DropTo::Prompt => SETTLE_AFTER + slow,
            DropTo::Sent { .. } => SETTLE_AFTER + normal,
        };
        let elapsed = cx
            .background_executor()
            .now()
            .saturating_duration_since(dropped.started);
        if elapsed >= total {
            return None;
        }
        let part = |after: Duration, length: Duration, easing: &Easing| {
            let into = elapsed.saturating_sub(after).as_secs_f32() / length.as_secs_f32();
            easing.sample(into.min(1.))
        };
        let back = matches!(dropped.to, DropTo::Back);
        Some(DropFrame {
            moved: part(Duration::ZERO, slow, &motion.easing_move),
            faded: if back {
                0.
            } else {
                part(FADE_AFTER, FADE, &motion.easing_enter)
            },
            filled: part(FADE_AFTER, normal, &motion.easing_enter),
            hollow: if back {
                1.
            } else {
                1. - part(SETTLE_AFTER, normal, &motion.easing_enter)
            },
            settling: elapsed >= SETTLE_AFTER,
        })
    }
}

/// What gpui draws under the pointer while a card is in hand: the card, lifting off the
/// board onto the menus' surface over the fast duration.
pub(super) struct CardInHand {
    note: StuffNote,
    width: Pixels,
    picked: Instant,
}

impl Render for CardInHand {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let motion = cx.theme().motion_tokens();
        let elapsed = cx
            .background_executor()
            .now()
            .saturating_duration_since(self.picked);
        let progress = elapsed.as_secs_f32() / motion.duration_fast.as_secs_f32();
        let lifted = if cx.reduce_motion() || progress >= 1. {
            1.
        } else {
            window.request_animation_frame();
            motion.easing_enter.sample(progress)
        };
        let look = MenuLook::new(cx);
        let card = CardLook::new(cx);
        div()
            .w(self.width)
            .p(CARD_PAD)
            .rounded(CARD_RADIUS)
            .bg(card.background.interpolate(&look.surface, lifted))
            .shadow(mix_shadows(
                &card.shadow,
                &in_hand_shadow(cx.theme().is_dark()),
                lifted,
            ))
            .text_color(cx.theme().foreground)
            .text_size(TEXT_SIZE)
            .line_height(LINE_HEIGHT)
            .child(card.body(&self.note))
    }
}

/// Makes a card's drag view as it is picked up, and keeps which card it is, where the
/// pointer holds it and how big it is, for when it is let go.
pub(super) fn pick_up(
    view: WeakEntity<ChatView>,
) -> impl Fn(&StuffDrag, Point<Pixels>, &mut Window, &mut App) -> Entity<CardInHand> + 'static {
    move |drag, grab, _, cx| {
        let picked = cx.background_executor().now();
        let held = view
            .update(cx, |this, cx| {
                let stuff = &mut this.stuff;
                let note = stuff.notes.iter().find(|note| note.id == drag.id)?.clone();
                let size = stuff.layout.cards.borrow().get(&note.id)?.size;
                stuff.drag.over = None;
                stuff.drag.held = Some(Held {
                    note: note.clone(),
                    grab,
                    size,
                });
                cx.notify();
                Some((note, size.width))
            })
            .ok()
            .flatten();
        // A card gone from the board meanwhile is held as nothing, and lands nowhere.
        let (note, width) = held.unwrap_or_else(|| {
            let note = StuffNote {
                id: drag.id,
                text: String::new(),
                attachments: Vec::new(),
            };
            (note, px(0.))
        });
        cx.new(|_| CardInHand {
            note,
            width,
            picked,
        })
    }
}

impl ChatView {
    /// The zone a card let go at `at` goes to: the prompt zone, then the rest of the chat.
    /// A card of pasted things alone has nothing the chat can take yet, so it goes back.
    fn zone_at(&self, at: Point<Pixels>) -> Option<Zone> {
        let drag = &self.stuff.drag;
        if drag
            .held
            .as_ref()
            .is_some_and(|held| held.note.text.is_empty())
        {
            return None;
        }
        let zones = &drag.zones;
        if zones.prompt.get().contains(&at) {
            Some(Zone::Prompt { busy: false })
        } else if zones.chat.get().contains(&at) {
            if self.sessions[self.active].busy {
                Some(Zone::Prompt { busy: true })
            } else {
                Some(Zone::History)
            }
        } else {
            None
        }
    }

    fn drag_over(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let zone = self.zone_at(at);
        let drag = &mut self.stuff.drag;
        if zone != drag.over {
            drag.over = zone;
            drag.last = zone.or(drag.last);
            cx.notify();
        }
    }

    /// The card in hand let go at `at`, over `zone`: into the prompt, sent, or back.
    fn let_go(
        &mut self,
        at: Point<Pixels>,
        zone: Option<Zone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(held) = self.stuff.drag.held.take() else {
            return;
        };
        let to = match zone {
            Some(Zone::Prompt { .. }) => {
                let key = self.sessions[self.active].key;
                let text = held.note.text.clone();
                if cx.reduce_motion() {
                    self.add_to_prompt(key, &text, window, cx);
                } else {
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(INTO_PROMPT_AFTER).await;
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.add_to_prompt(key, &text, window, cx)
                        });
                    })
                    .detach();
                }
                DropTo::Prompt
            }
            Some(Zone::History) => {
                self.send_prompt(held.note.text.clone(), cx);
                let session = &self.sessions[self.active];
                DropTo::Sent {
                    key: session.key,
                    row: session.rows.len() - 1,
                }
            }
            None => DropTo::Back,
        };
        self.stuff.drag.zones.sent.set(None);
        self.stuff.drag.dropped = (!cx.reduce_motion()).then(|| Dropped {
            from: Bounds::new(at - held.grab, held.size),
            note: held.note,
            to,
            started: cx.background_executor().now(),
        });
        cx.notify();
    }

    /// Puts `text` at the end of the session's prompt after a blank line, the caret after
    /// it and the keys in the prompt, without sending it. A session gone off screen
    /// meanwhile gets it at the end of its kept draft.
    fn add_to_prompt(
        &mut self,
        key: SessionKey,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions[self.active].key != key {
            if let Some(session) = self.session_mut(key) {
                session.draft = appended(&session.draft, text).into();
            }
            return;
        }
        self.input.update(cx, |input, cx| {
            let value = input.value();
            let kept = value.trim_end().len();
            let added = if kept == 0 {
                text.to_string()
            } else {
                format!("\n\n{text}")
            };
            // In place of any blank lines at the end, as one change to undo.
            input.set_selected_range(kept..value.len(), cx);
            input.replace(added, window, cx);
            input.focus(window, cx);
        });
    }

    /// The window's root, taking cards let go anywhere over it or outside it, and Esc.
    pub(crate) fn take_stuff_drops(&self, root: Div, cx: &mut Context<Self>) -> Div {
        let zones = self.stuff.drag.zones.clone();
        root.child(measure(move |bounds| zones.root.set(bounds)))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<StuffDrag>, _, cx| {
                    this.drag_over(event.event.position, cx)
                }),
            )
            .on_drop(cx.listener(|this, _: &StuffDrag, window, cx| {
                let at = window.mouse_position();
                this.let_go(at, this.zone_at(at), window, cx)
            }))
            // Let go outside the window. gpui drops the drag itself after this.
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if this.stuff.drag.dragging(cx) {
                        this.let_go(event.position, None, window, cx);
                    }
                }),
            )
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.stuff.drag.dragging(cx) {
                    cx.stop_active_drag(window);
                    this.let_go(window.mouse_position(), None, window, cx);
                    cx.stop_propagation();
                }
            }))
    }

    /// The card let go of, on its way, over everything else in the window.
    pub(crate) fn render_card_drop(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let drag = &self.stuff.drag;
        let frame = drag.drop_frame(cx)?;
        // Past the card itself: the card on the board and the prompt's ring after it.
        window.request_animation_frame();
        let dropped = drag.dropped.as_ref()?;
        if frame.faded >= 1. {
            return None;
        }
        let from = dropped.from;
        let corner = match dropped.to {
            DropTo::Prompt => Some(drag.zones.prompt.get().origin + INTO_PROMPT),
            DropTo::Sent { .. } => drag
                .zones
                .sent
                .get()
                .map(|row| point(row.right() - from.size.width, row.top())),
            DropTo::Back => self
                .stuff
                .layout
                .cards
                .borrow()
                .get(&dropped.note.id)
                .map(|card| card.origin),
        };
        // Until its place has been laid out, it stays where it was let go.
        let to = Bounds::new(corner.unwrap_or(from.origin), from.size);
        let bounds = from.interpolate(&to, frame.moved);
        let origin = bounds.origin - drag.zones.root.get().origin;
        let look = MenuLook::new(cx);
        let card = CardLook::new(cx);
        let lifted = in_hand_shadow(cx.theme().is_dark());
        // Back on the board it settles as it lands; anywhere else it stays lifted.
        let settled = if matches!(dropped.to, DropTo::Back) {
            frame.moved
        } else {
            0.
        };
        Some(
            div()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .w(bounds.size.width)
                .h(bounds.size.height)
                .p(CARD_PAD)
                .rounded(CARD_RADIUS)
                .bg(look.surface.interpolate(&card.background, settled))
                .shadow(mix_shadows(&lifted, &card.shadow, settled))
                .text_color(cx.theme().foreground)
                .text_size(TEXT_SIZE)
                .line_height(LINE_HEIGHT)
                .opacity(1. - frame.faded)
                .child(card.body(&dropped.note)),
        )
    }

    /// The prompt zone, measured, with the pill that says what letting go does there.
    pub(crate) fn prompt_drop_zone(
        &self,
        zone: Div,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let drag = &self.stuff.drag;
        let lit = matches!(drag.over(cx), Some(Zone::Prompt { .. }));
        let busy = matches!(drag.last, Some(Zone::Prompt { busy: true }));
        let zones = drag.zones.clone();
        let fast = cx.theme().motion_tokens().duration_fast;
        let shown = self.zone_light(("stuff-zone", "prompt-pill"), lit, fast, window, cx);
        let rise = cx
            .theme()
            .motion_tokens()
            .distance_short
            .to_pixels(window.rem_size());
        let text = if busy {
            "Agent is working · release to add to the prompt"
        } else {
            "Release to add to the prompt"
        };
        zone.relative()
            .child(measure(move |bounds| zones.prompt.set(bounds)))
            .when(shown > 0., |zone| {
                zone.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(PROMPT_ZONE_PAD - PILL_GAP - PILL_HEIGHT + rise * (1. - shown))
                        .flex()
                        .justify_center()
                        .opacity(shown)
                        .child(pill(text, cx)),
                )
            })
    }

    /// The prompt box's edge: its 1px border, or the ring it lights while a card is
    /// over it, kept till a card let go there is in.
    pub(crate) fn prompt_edge(
        &self,
        border: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<BoxShadow> {
        let drag = &self.stuff.drag;
        let over = matches!(drag.over(cx), Some(Zone::Prompt { .. }));
        let frame = drag
            .dropped
            .as_ref()
            .filter(|dropped| matches!(dropped.to, DropTo::Prompt))
            .and_then(|_| drag.drop_frame(cx));
        let going_in = frame.as_ref().is_some_and(|frame| !frame.settling);
        let motion = cx.theme().motion_tokens();
        let out = if frame.is_some() {
            motion.duration_slow
        } else {
            motion.duration_fast
        };
        let lit = self.zone_light(("stuff-zone", "prompt"), over || going_in, out, window, cx);
        let foreground = cx.theme().foreground;
        let mut edge = vec![
            BoxShadow::new(
                px(0.),
                px(0.),
                border.interpolate(&foreground.opacity(RING_ALPHA), lit),
            )
            .spread_radius(px(1.).interpolate(&RING_WIDTH, lit))
            .inset(),
        ];
        if lit > 0. {
            edge.push(
                BoxShadow::new(px(0.), px(0.), foreground.opacity(HALO_ALPHA * lit))
                    .spread_radius(HALO_WIDTH),
            );
        }
        edge
    }

    /// The chat, measured, with the veil over its transcript while a card would be sent
    /// from there.
    pub(crate) fn chat_drop_zone(
        &self,
        chat: Div,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let drag = &self.stuff.drag;
        let lit = drag.over(cx) == Some(Zone::History);
        let sent = drag
            .dropped
            .as_ref()
            .is_some_and(|dropped| matches!(dropped.to, DropTo::Sent { .. }));
        let motion = cx.theme().motion_tokens();
        let out = if sent {
            motion.duration_normal
        } else {
            motion.duration_fast
        };
        let shown = self.zone_light(("stuff-zone", "history"), lit, out, window, cx);
        let zones = drag.zones.clone();
        let foreground = cx.theme().foreground;
        let prompt_height = drag.zones.prompt.get().size.height;
        let rise = cx
            .theme()
            .motion_tokens()
            .distance_short
            .to_pixels(window.rem_size());
        chat.relative()
            .child(measure(move |bounds| zones.chat.set(bounds)))
            .when(shown > 0., |chat| {
                chat.child(
                    div()
                        .absolute()
                        .left(VEIL_INSET)
                        .right(VEIL_INSET)
                        .top(VEIL_TOP)
                        .bottom(prompt_height)
                        .rounded(VEIL_RADIUS)
                        .bg(foreground.opacity(VEIL_ALPHA))
                        .shadow(vec![
                            BoxShadow::new(px(0.), px(0.), foreground.opacity(RING_ALPHA))
                                .spread_radius(RING_WIDTH)
                                .inset(),
                            BoxShadow::new(px(0.), px(0.), foreground.opacity(HALO_ALPHA))
                                .spread_radius(HALO_WIDTH),
                        ])
                        .opacity(shown)
                        .flex()
                        .items_end()
                        .justify_center()
                        .pb(VEIL_PILL_BOTTOM - rise * (1. - shown))
                        .child(pill("Release to send", cx)),
                )
            })
    }

    /// How lit a zone is: easing in over the fast duration as a card comes over it, and
    /// out over `out`, which is longer once a card has been let go there.
    fn zone_light(
        &self,
        id: (&'static str, &'static str),
        lit: bool,
        out: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> f32 {
        let motion = cx.theme().motion_tokens();
        let duration = if lit { motion.duration_fast } else { out };
        transition(
            id,
            if lit { 1. } else { 0. },
            Transition::new(duration).easing(motion.easing_enter.clone()),
            window,
            cx,
        )
    }

    /// The bubble of the row at `row` in `key`'s transcript, as it fills in under the card
    /// it was sent as: a dashed outline its size at first.
    pub(crate) fn sent_bubble(&self, key: SessionKey, row: usize, bubble: Div, cx: &App) -> Div {
        let drag = &self.stuff.drag;
        let sent_here = drag.dropped.as_ref().is_some_and(|dropped| {
            matches!(dropped.to, DropTo::Sent { key: sent, row: at } if sent == key && at == row)
        });
        let Some(frame) = drag.drop_frame(cx).filter(|_| sent_here) else {
            return bubble;
        };
        let theme = cx.theme();
        let zones = drag.zones.clone();
        let edge = theme
            .foreground
            .opacity(SLOT_EDGE_ALPHA * (1. - frame.filled));
        bubble
            .bg(theme.secondary.opacity(frame.filled))
            .text_color(theme.foreground.opacity(frame.filled))
            .child(dashed_edge(edge).rounded_lg())
            .child(measure(move |bounds| zones.sent.set(Some(bounds))))
    }
}

/// `draft` with `text` after it and a blank line between, or just `text` if there is no
/// draft.
fn appended(draft: &str, text: &str) -> String {
    let draft = draft.trim_end();
    if draft.is_empty() {
        text.to_string()
    } else {
        format!("{draft}\n\n{text}")
    }
}

/// What letting go would do, on the menus' surface.
fn pill(text: &'static str, cx: &App) -> Div {
    let look = MenuLook::new(cx);
    div()
        .h(PILL_HEIGHT)
        .px(PILL_PAD_X)
        .flex()
        .items_center()
        .rounded(PILL_HEIGHT / 2.)
        .bg(look.surface)
        .shadow(look.shadow_with_edge())
        .text_color(look.foreground)
        .text_size(PILL_TEXT_SIZE)
        .whitespace_nowrap()
        .child(text)
}

/// A card in hand: higher off the board than the menus, with the same ring, two wider
/// soft shadows and, in dark mode, the same edge. GPUI's blur is half of CSS's, so 24
/// and 8 here are CSS's 48 and 16.
fn in_hand_shadow(dark: bool) -> Vec<BoxShadow> {
    let ink = |alpha| black().opacity(alpha);
    let (ring, far, near, far_spread) = if dark {
        (0.6, 0.7, 0.45, -8.)
    } else {
        (0.09, 0.22, 0.12, -10.)
    };
    let mut shadow = vec![
        BoxShadow::new(px(0.), px(0.), ink(ring)).spread_radius(px(1.)),
        BoxShadow::new(px(0.), px(24.), ink(far))
            .blur_radius(px(24.))
            .spread_radius(px(far_spread)),
        BoxShadow::new(px(0.), px(8.), ink(near))
            .blur_radius(px(8.))
            .spread_radius(px(-4.)),
    ];
    if dark {
        shadow.push(
            BoxShadow::new(px(0.), px(0.), white().opacity(0.07))
                .spread_radius(px(1.))
                .inset(),
        );
    }
    shadow
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::appended;

    #[test]
    fn adds_after_a_blank_line() {
        assert_eq!(appended("", "note"), "note");
        assert_eq!(appended("  \n", "note"), "note");
        assert_eq!(appended("draft", "note"), "draft\n\nnote");
        assert_eq!(appended("draft\n\n\n", "a\nb"), "draft\n\na\nb");
    }
}
