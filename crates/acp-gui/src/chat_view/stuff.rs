//! The Stuff panel, the sidebar's other list: a note in front to write or paste anything
//! into, and the board behind it that each finished note is stuck onto. It belongs to the
//! window, not to a session, so switching sessions leaves the note and the board as they
//! are. A stuck note flies onto the board and turns into its card on the way, and a card
//! can be dragged into the chat (see `drag`).

mod drag;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::base::{Easing, Interpolate as _};
use gpui_kit::component::input::{Enter, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::{MenuLook, SURFACE_RADIUS};
use crate::chat_view::stuff::drag::{DragState, StuffDrag, pick_up};
use crate::config::{StuffNote, change_stuff, config_folder, load_stuff};

/// The text of the note and the cards. Both wrap it at the sidebar's width less 40, so a
/// note's lines break in the same places once it is a card.
const TEXT_SIZE: Pixels = px(13.);
const LINE_HEIGHT: Pixels = px(18.);
/// The note grows from 3 lines to 14, then scrolls inside.
const NOTE_MIN_LINES: usize = 3;
const NOTE_MAX_LINES: usize = 14;
/// The note sits 8px in from the sidebar's sides and below the tabs, like the session
/// cards. It is on the menus' surface, as round as they are.
const NOTE_INSET: Pixels = px(8.);
/// The note's padding around its text: the field's own 4px at the sides makes 12.
const NOTE_PAD_TOP: Pixels = px(10.);
const NOTE_PAD_X: Pixels = px(8.);
const NOTE_PAD_BOTTOM: Pixels = px(8.);
const FIELD_PAD_X: Pixels = px(4.);
/// The row below the text with the shortcut and the Stick button.
const STICK_ROW_HEIGHT: Pixels = px(24.);
const STICK_ROW_GAP: Pixels = px(6.);
const STICK_ICON_SIZE: Pixels = px(14.);
const STICK_TEXT_SIZE: Pixels = px(12.);
const SHORTCUT_TEXT_SIZE: Pixels = px(11.);
/// The board starts 12px below the note. Each card is 4px further in than the note at
/// either side, with 4px less padding, so its text keeps the note's width.
const BOARD_PAD: Pixels = px(12.);
const BOARD_PAD_BOTTOM: Pixels = px(4.);
const CARD_GAP: Pixels = px(8.);
const CARD_PAD: Pixels = px(8.);
const CARD_RADIUS: Pixels = px(8.);
/// A card shows its first 6 lines.
const CARD_LINES: usize = 6;
/// The strip of tape across a card's top edge, half above it. A new card's drops the last
/// 3px onto it.
const TAPE_WIDTH: Pixels = px(32.);
const TAPE_HEIGHT: Pixels = px(10.);
const TAPE_RADIUS: Pixels = px(2.);
const TAPE_DROP: Pixels = px(3.);
/// The place a card in hand leaves on the board: a dashed edge at the text's color, 20%,
/// with the tape still on it.
const SLOT_EDGE_ALPHA: f32 = 0.2;
/// The board's dots: 2px across, one every 12px.
const DOT_SPACING: Pixels = px(12.);
const DOT_SIZE: Pixels = px(2.);
const DOT_OFFSET: Pixels = px(1.);
/// The empty board's line, a little way down.
const EMPTY_TOP: Pixels = px(40.);
/// Sticking a note takes 420ms with the theme's durations. The note flies to the board's
/// first slot over the slow one while its shadow presses flat, and the shadow settles
/// over the fast one after. The new empty note comes up from 100ms and the card's tape
/// drops from 240ms, each over the normal one.
const NEW_NOTE_AFTER: Duration = Duration::from_millis(100);
const TAPE_AFTER: Duration = Duration::from_millis(240);

/// The note and the board, kept for the whole window.
pub(crate) struct StuffState {
    /// Newest first, as on the board.
    notes: Vec<StuffNote>,
    /// The note being written, in front of the board.
    pub(crate) input: Entity<TextareaState>,
    _input_events: Subscription,
    /// The note last stuck, flying onto the board until its time is up.
    sticking: Option<Sticking>,
    layout: Rc<StuffLayout>,
    /// Back at the top for each note stuck, where its card goes.
    board_scroll: ScrollHandle,
    drag: DragState,
}

/// A note stuck onto the board, from where it was in front.
struct Sticking {
    /// The note, now the newest card.
    id: u64,
    /// Where the note was in the panel.
    from: Bounds<Pixels>,
    started: Instant,
}

/// Where the panel, the note and each card were last laid out, in the window: what a
/// note flying onto the board, or a card going back to it, starts from and lands on.
#[derive(Default)]
struct StuffLayout {
    panel: Cell<Bounds<Pixels>>,
    note: Cell<Bounds<Pixels>>,
    cards: RefCell<HashMap<u64, Bounds<Pixels>>>,
}

/// One frame of a note being stuck: how far along each part is, eased, 0 to 1.
#[derive(Clone, Copy)]
struct StickFrame {
    id: u64,
    from: Bounds<Pixels>,
    /// The note flying to the card's place, and the slots growing and shrinking with it.
    moved: f32,
    /// Once moved, the card on the board takes over from the flying note.
    landed: bool,
    /// The card's shadow settling after it lands.
    settled: f32,
    /// What only the note has fading out: its lines past the card's and its Stick row.
    faded: f32,
    /// The new empty note coming up in front.
    shown: f32,
    /// The card's tape dropping onto it.
    taped: f32,
}

impl StuffState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<ChatView>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Write or paste anything…")
                .auto_grow(NOTE_MIN_LINES, NOTE_MAX_LINES)
        });
        // The Stick button is dim while the note is empty.
        let input_events = cx.subscribe_in(&input, window, |_, _, event, _, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        Self {
            notes: load_stuff(),
            input,
            _input_events: input_events,
            sticking: None,
            layout: Rc::default(),
            board_scroll: ScrollHandle::new(),
            drag: DragState::default(),
        }
    }

    /// Where the note last stuck is on its way to the board, until it is there.
    fn stick_frame(&self, cx: &App) -> Option<StickFrame> {
        let sticking = self.sticking.as_ref()?;
        let motion = cx.theme().motion_tokens();
        let (fast, normal, slow) = (
            motion.duration_fast,
            motion.duration_normal,
            motion.duration_slow,
        );
        let total = (slow + fast)
            .max(NEW_NOTE_AFTER + normal)
            .max(TAPE_AFTER + normal);
        let elapsed = cx
            .background_executor()
            .now()
            .saturating_duration_since(sticking.started);
        if elapsed >= total {
            return None;
        }
        // A part that starts `after` the stick and takes `length`.
        let part = |after: Duration, length: Duration, easing: &Easing| {
            let into = elapsed.saturating_sub(after).as_secs_f32() / length.as_secs_f32();
            easing.sample(into.min(1.))
        };
        Some(StickFrame {
            id: sticking.id,
            from: sticking.from,
            moved: part(Duration::ZERO, slow, &motion.easing_move),
            landed: elapsed >= slow,
            settled: part(slow, fast, &motion.easing_enter),
            faded: part(Duration::ZERO, fast, &motion.easing_enter),
            shown: part(NEW_NOTE_AFTER, normal, &motion.easing_enter),
            taped: part(TAPE_AFTER, normal, &motion.easing_enter),
        })
    }

    /// An id for a note stuck now: the time, moved past the newest note's if the clock
    /// gives the same or an earlier one.
    fn next_id(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_millis() as u64);
        let newest = self.notes.iter().map(|note| note.id).max().unwrap_or(0);
        now.max(newest + 1)
    }
}

impl ChatView {
    /// Sticks the note onto the board, newest first, and keeps it for later runs. The
    /// note flies there and empties, ready for the next one, which can be written at
    /// once but sticks only once this one has landed. An empty note sticks nothing.
    fn stick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.stuff.input.read(cx).value().trim_end().to_string();
        if text.trim().is_empty() || self.stuff.stick_frame(cx).is_some() {
            return;
        }
        let note = StuffNote {
            id: self.stuff.next_id(),
            text,
        };
        let layout = &self.stuff.layout;
        let (panel, from) = (layout.panel.get(), layout.note.get());
        self.stuff.sticking = (!cx.reduce_motion()).then(|| Sticking {
            id: note.id,
            from: Bounds::new(from.origin - panel.origin, from.size),
            started: cx.background_executor().now(),
        });
        self.stuff.board_scroll.set_offset(Point::default());
        self.stuff.notes.insert(0, note.clone());
        self.stuff
            .input
            .update(cx, |input, cx| input.clean(window, cx));
        let changed = config_folder()
            .ok_or_else(|| std::io::Error::other("there is no config folder"))
            .and_then(|folder| change_stuff(&folder, |notes| notes.insert(0, note)));
        if let Err(err) = changed {
            eprintln!("couldn't keep the note for later runs: {err}");
        }
        cx.notify();
    }

    /// The Stuff panel: the note in front, the board behind it, and a note flying from
    /// one to the other over both.
    pub(crate) fn render_stuff(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let frame = self.stuff.stick_frame(cx);
        if frame.is_some() {
            window.request_animation_frame();
        }
        let dots = if cx.theme().is_dark() {
            white().opacity(0.06)
        } else {
            black().opacity(0.08)
        };
        let layout = self.stuff.layout.clone();
        div()
            .flex_1()
            .min_h(px(0.))
            .relative()
            .flex()
            .flex_col()
            .child(measure(move |bounds| layout.panel.set(bounds)))
            .child(dot_grid(dots))
            .child(self.render_note(frame, window, cx))
            .child(self.render_board(frame, cx))
            .children(frame.and_then(|frame| self.render_flight(frame, cx)))
    }

    /// The note in front of the board, on the menus' surface: the field, and a row with
    /// the shortcut and the button that stick it. Cmd+Enter sticks it from the field,
    /// where Enter alone starts a new line. While a note flies off, its place shrinks to
    /// the new one's height, bringing the board up, and the new one comes up into it.
    fn render_note(
        &self,
        frame: Option<StickFrame>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let look = MenuLook::new(cx);
        let empty = self.stuff.input.read(cx).value().trim().is_empty();
        let rise = cx
            .theme()
            .motion_tokens()
            .distance_short
            .to_pixels(window.rem_size());
        let layout = self.stuff.layout.clone();
        let note = look
            .surface()
            .id("stuff-note")
            .pt(NOTE_PAD_TOP)
            .px(NOTE_PAD_X)
            .pb(NOTE_PAD_BOTTOM)
            .flex()
            .flex_col()
            // Before the field's own Enter, which would put a new line in first.
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if action.secondary {
                    cx.stop_propagation();
                    this.stick(window, cx);
                }
            }))
            .child(
                Textarea::new(&self.stuff.input)
                    .appearance(false)
                    // The smallest size has no padding above the text and 4px at its
                    // sides, which the note's own padding makes up to the design's.
                    .with_size(Size::XSmall)
                    .text_size(TEXT_SIZE)
                    .line_height(LINE_HEIGHT),
            )
            .child(self.stick_row(empty, &look, cx).mt(STICK_ROW_GAP))
            .children(look.edge())
            .child(measure(move |bounds| layout.note.set(bounds)))
            .when_some(frame, |note, frame| {
                note.top(rise * (1. - frame.shown)).opacity(frame.shown)
            });
        div()
            .flex_none()
            .pt(NOTE_INSET)
            .px(NOTE_INSET)
            .when_some(frame, |place, frame| {
                let height = self.stuff.layout.note.get().size.height;
                place.h(NOTE_INSET + frame.from.size.height.interpolate(&height, frame.moved))
            })
            .child(note)
    }

    /// The row below the note's text: the shortcut, once there is something to stick,
    /// and the Stick button.
    fn stick_row(&self, empty: bool, look: &MenuLook, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let shortcut = if cfg!(target_os = "macos") {
            "⌘↩"
        } else {
            "Ctrl+↩"
        };
        let hover = look.hover;
        let stick = div()
            .id("stick")
            .role(Role::Button)
            .aria_label("Stick")
            .h(STICK_ROW_HEIGHT)
            .pl(px(6.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(6.))
            .text_size(STICK_TEXT_SIZE)
            .child(Icon::new(IconName::Pin).size(STICK_ICON_SIZE))
            .child("Stick")
            .map(|button| {
                if empty {
                    button.opacity(0.4)
                } else {
                    button
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| this.stick(window, cx)),
                        )
                }
            });
        div()
            .h(STICK_ROW_HEIGHT)
            .flex()
            .items_center()
            .justify_end()
            .gap(px(6.))
            .child(
                div()
                    .text_size(SHORTCUT_TEXT_SIZE)
                    .text_color(muted)
                    .when(empty, |hint| hint.opacity(0.))
                    .child(shortcut),
            )
            .child(stick)
    }

    /// The board: the stuck notes as cards, newest at the top, scrolling under the note.
    /// The card a note is flying to grows its place from nothing, pushing the others
    /// down, and shows once the note lands on it. A card can be picked up and dragged
    /// into the chat, leaving its place behind.
    fn render_board(&self, frame: Option<StickFrame>, cx: &mut Context<Self>) -> Stateful<Div> {
        let look = CardLook::new(cx);
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let cards = self.stuff.notes.iter().map(|note| {
            let id = note.id;
            let layout = self.stuff.layout.clone();
            let flying = frame.filter(|frame| frame.id == id);
            let (shadow, taped) = match flying {
                Some(frame) => (
                    mix_shadows(&flattened(&look.shadow), &look.shadow, frame.settled),
                    frame.taped,
                ),
                None => (look.shadow.clone(), 1.),
            };
            let card = look
                .card(note, shadow, taped, self.stuff.drag.hollow(id, cx))
                .child(measure(move |bounds| {
                    layout.cards.borrow_mut().insert(id, bounds);
                }))
                .on_drag(StuffDrag::new(id), pick_up(view.clone()));
            let Some(frame) = flying else {
                return card.into_any_element();
            };
            let height = self
                .stuff
                .layout
                .cards
                .borrow()
                .get(&id)
                .map_or(px(0.), |card| card.size.height);
            let card = card.when(!frame.landed, |card| card.opacity(0.));
            // Less the gap after it while it has no height, so the cards below move
            // only as it grows.
            div()
                .flex_none()
                .h(height * frame.moved)
                .mb(-CARD_GAP * (1. - frame.moved))
                .child(card)
                .into_any_element()
        });
        // The empty board's line, fading as the first note flies in.
        let empty = match frame {
            Some(frame) if self.stuff.notes.len() == 1 => Some(1. - frame.faded),
            None if self.stuff.notes.is_empty() => Some(1.),
            _ => None,
        };
        div()
            .id("stuff-board")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.stuff.board_scroll)
            .pt(BOARD_PAD)
            .px(BOARD_PAD)
            .pb(BOARD_PAD_BOTTOM)
            .flex()
            .flex_col()
            .gap(CARD_GAP)
            .children(cards)
            .when_some(empty, |board, shown| {
                board.child(
                    div()
                        .pt(EMPTY_TOP)
                        .text_center()
                        .text_xs()
                        .text_color(muted)
                        .opacity(shown)
                        .child("Stuck notes land here"),
                )
            })
    }

    /// The note on its way onto the board, over the panel: the note's box turning into
    /// the card's, while the lines past the card's and the Stick row fade out. The text
    /// stays put inside, as the box comes in 4px at each side and its padding goes out 4.
    fn render_flight(&self, frame: StickFrame, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if frame.landed {
            return None;
        }
        let note = self.stuff.notes.iter().find(|note| note.id == frame.id)?;
        let look = MenuLook::new(cx);
        let card = CardLook::new(cx);
        let layout = &self.stuff.layout;
        // The board's first place, under the note as it is once empty. Until the card
        // has been laid out, the note keeps its height.
        let from = frame.from;
        let inset = BOARD_PAD - NOTE_INSET;
        let to = Bounds::new(
            point(
                from.left() + inset,
                from.top() + layout.note.get().size.height + BOARD_PAD,
            ),
            size(
                from.size.width - inset * 2.,
                layout
                    .cards
                    .borrow()
                    .get(&frame.id)
                    .map_or(from.size.height, |card| card.size.height),
            ),
        );
        let bounds = from.interpolate(&to, frame.moved);
        let moved = |from: Pixels, to: Pixels| from.interpolate(&to, frame.moved);
        let card_lines = LINE_HEIGHT * CARD_LINES as f32;
        // The card's text, and over it the note's lines past the card's: the whole
        // text moved up by the card's lines, cut off above them.
        let text = div()
            .relative()
            .child(card_text(note, card.link_line))
            .child(
                div()
                    .absolute()
                    .top(card_lines)
                    .left_0()
                    .right_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .opacity(1. - frame.faded)
                    .child(div().mt(-card_lines).child(note.text.clone())),
            );
        Some(
            div()
                .id("stuff-flight")
                .absolute()
                .left(bounds.left())
                .top(bounds.top())
                .w(bounds.size.width)
                .h(bounds.size.height)
                .rounded(moved(SURFACE_RADIUS, CARD_RADIUS))
                .bg(look.surface.interpolate(&card.background, frame.moved))
                .shadow(mix_shadows(
                    &look.shadow_with_edge(),
                    &flattened(&card.shadow),
                    frame.moved,
                ))
                .text_size(TEXT_SIZE)
                .line_height(LINE_HEIGHT)
                .child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .pt(moved(NOTE_PAD_TOP, CARD_PAD))
                        .px(moved(NOTE_PAD_X + FIELD_PAD_X, CARD_PAD))
                        .child(text),
                )
                .child(
                    self.stick_row(false, &look, cx)
                        .absolute()
                        .right(NOTE_PAD_X)
                        .bottom(NOTE_PAD_BOTTOM)
                        .opacity(1. - frame.faded),
                )
                .child(card.tape(frame.taped)),
        )
    }
}

/// How the board's cards look.
struct CardLook {
    background: Hsla,
    tape: Hsla,
    link_line: Hsla,
    /// The dashed edge of the place a card in hand leaves.
    slot_edge: Hsla,
    /// At rest on the board.
    shadow: Vec<BoxShadow>,
}

impl CardLook {
    fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let dark = theme.is_dark();
        let (background, tape, link_line) = if dark {
            (
                rgb(0x1a1a1a).into(),
                white().opacity(0.15),
                theme.foreground.opacity(0.4),
            )
        } else {
            (
                white(),
                black().opacity(0.08),
                theme.foreground.opacity(0.35),
            )
        };
        Self {
            background,
            tape,
            link_line,
            slot_edge: theme.foreground.opacity(SLOT_EDGE_ALPHA),
            shadow: card_shadow(dark),
        }
    }

    /// A card, with `shadow`, its tape `taped` of the way down onto it. While it is
    /// `hollow`, it is only its place: a dashed edge and the tape.
    fn card(
        &self,
        note: &StuffNote,
        shadow: Vec<BoxShadow>,
        taped: f32,
        hollow: f32,
    ) -> Stateful<Div> {
        div()
            .id(("stuff-card", note.id))
            .relative()
            .flex_none()
            .p(CARD_PAD)
            .rounded(CARD_RADIUS)
            .bg(self.background.opacity(1. - hollow))
            .shadow(dimmed(&shadow, 1. - hollow))
            .text_size(TEXT_SIZE)
            .line_height(LINE_HEIGHT)
            .child(self.tape(taped))
            .child(
                card_text(note, self.link_line).when(hollow > 0., |text| text.opacity(1. - hollow)),
            )
            // Over the card rather than its own border, which would move its content.
            .when(hollow > 0., |card| {
                card.child(dashed_edge(self.slot_edge.opacity(hollow)).rounded(CARD_RADIUS))
            })
    }

    /// The strip of tape across a card's top edge, half above it, `dropped` of the way
    /// down onto it and as faded in.
    fn tape(&self, dropped: f32) -> Div {
        div()
            .absolute()
            .top(-TAPE_HEIGHT / 2. - TAPE_DROP * (1. - dropped))
            .left_1_2()
            .ml(-TAPE_WIDTH / 2.)
            .w(TAPE_WIDTH)
            .h(TAPE_HEIGHT)
            .rounded(TAPE_RADIUS)
            .bg(self.tape)
            .opacity(dropped)
    }
}

/// `shadow` pressed flat against the board: its soft shadows gone, its ring and edge
/// kept.
fn flattened(shadow: &[BoxShadow]) -> Vec<BoxShadow> {
    shadow
        .iter()
        .map(|layer| {
            if layer.blur_radius > px(0.) {
                BoxShadow::new(
                    px(0.),
                    px(0.),
                    Hsla {
                        a: 0.,
                        ..layer.color
                    },
                )
            } else {
                layer.clone()
            }
        })
        .collect()
}

/// A dashed edge just inside its parent, in `color`.
fn dashed_edge(color: Hsla) -> Div {
    div()
        .absolute()
        .inset_0()
        .border_1()
        .border_dashed()
        .border_color(color)
}

/// `shadow` with each layer `by` as strong.
fn dimmed(shadow: &[BoxShadow], by: f32) -> Vec<BoxShadow> {
    shadow
        .iter()
        .map(|layer| BoxShadow {
            color: layer.color.opacity(by),
            ..layer.clone()
        })
        .collect()
}

/// Each of `from`'s shadows `progress` of the way to the same one of `to`'s. Both lists
/// are a ring, two soft shadows and, in dark mode, an edge.
fn mix_shadows(from: &[BoxShadow], to: &[BoxShadow], progress: f32) -> Vec<BoxShadow> {
    from.iter()
        .zip(to)
        .map(|(from, to)| BoxShadow {
            color: from.color.interpolate(&to.color, progress),
            offset: from.offset.interpolate(&to.offset, progress),
            blur_radius: from.blur_radius.interpolate(&to.blur_radius, progress),
            spread_radius: from.spread_radius.interpolate(&to.spread_radius, progress),
            inset: from.inset,
        })
        .collect()
}

/// A card's resting shadow: a 1px ring, two small soft shadows, and in dark mode a faint
/// light edge just inside. GPUI's blur is half of CSS's (see `MenuLook::shadow`).
fn card_shadow(dark: bool) -> Vec<BoxShadow> {
    let ink = |alpha| black().opacity(alpha);
    let (ring, far, near) = if dark {
        (0.5, 0.35, 0.4)
    } else {
        (0.07, 0.06, 0.05)
    };
    let mut shadow = vec![
        BoxShadow::new(px(0.), px(0.), ink(ring)).spread_radius(px(1.)),
        BoxShadow::new(px(0.), px(2.), ink(far))
            .blur_radius(px(2.))
            .spread_radius(px(-1.)),
        BoxShadow::new(px(0.), px(1.), ink(near)).blur_radius(px(1.)),
    ];
    if dark {
        shadow.push(
            BoxShadow::new(px(0.), px(0.), white().opacity(0.05))
                .spread_radius(px(1.))
                .inset(),
        );
    }
    shadow
}

/// A card's text, its first `CARD_LINES` lines, with each web address underlined and
/// opening in the browser on a click.
fn card_text(note: &StuffNote, link_line: Hsla) -> Div {
    let text = SharedString::from(note.text.clone());
    let ranges = links(&text);
    let clamp = div().line_clamp(CARD_LINES).text_ellipsis();
    if ranges.is_empty() {
        return clamp.child(text);
    }
    let underline = HighlightStyle {
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(link_line),
            wavy: false,
        }),
        ..Default::default()
    };
    let urls: Vec<String> = ranges
        .iter()
        .map(|range| text[range.clone()].to_string())
        .collect();
    let styled = StyledText::new(text.clone())
        .with_highlights(ranges.iter().map(|range| (range.clone(), underline)));
    clamp.child(
        InteractiveText::new(("stuff-text", note.id), styled)
            .on_click(ranges, move |ix, _, cx| cx.open_url(&urls[ix])),
    )
}

/// Where the web addresses in `text` are: each `http://` or `https://` up to the next
/// space, less the punctuation a sentence or brackets put after it.
fn links(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(start) = next_scheme(&text[from..]).map(|ix| from + ix) {
        let rest = &text[start..];
        let mut end = start + rest.find(char::is_whitespace).unwrap_or(rest.len());
        end = start
            + text[start..end]
                .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\''])
                .len();
        // Past the scheme, there must be something left.
        let scheme_len = if rest.starts_with("https://") { 8 } else { 7 };
        if end > start + scheme_len {
            found.push(start..end);
        }
        from = end.max(start + scheme_len);
    }
    found
}

/// Where the next `http://` or `https://` starts in `text`.
fn next_scheme(text: &str) -> Option<usize> {
    let http = text.find("http://");
    let https = text.find("https://");
    match (http, https) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Hands `keep` the bounds its parent was laid out at, each frame. `ElementExt`'s
/// `on_prepaint` starts inside the parent's padding, so its origin is off for a padded
/// parent.
fn measure(keep: impl FnOnce(Bounds<Pixels>) + 'static) -> impl IntoElement {
    canvas(move |bounds, _, _| keep(bounds), |_, _, _, _| {})
        .absolute()
        .inset_0()
}

/// The board's dots, painted behind the note and the cards.
fn dot_grid(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let mut y = bounds.top() + DOT_OFFSET;
            while y < bounds.bottom() {
                let mut x = bounds.left() + DOT_OFFSET;
                while x < bounds.right() {
                    let dot = Bounds::new(point(x, y), size(DOT_SIZE, DOT_SIZE));
                    window.paint_quad(fill(dot, color).corner_radii(DOT_SIZE / 2.));
                    x += DOT_SPACING;
                }
                y += DOT_SPACING;
            }
        },
    )
    .absolute()
    .inset_0()
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::links;

    fn found(text: &str) -> Vec<&str> {
        links(text).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn finds_web_addresses() {
        assert_eq!(
            found("Read later: https://agentclientprotocol.com/protocol/content"),
            ["https://agentclientprotocol.com/protocol/content"]
        );
        assert_eq!(
            found("see http://a.com, and (https://b.org/x)."),
            ["http://a.com", "https://b.org/x"]
        );
        assert_eq!(
            found("two\nhttps://a.com\nhttps://b.com"),
            ["https://a.com", "https://b.com"]
        );
    }

    #[test]
    fn skips_a_bare_scheme() {
        assert!(found("type https:// and then").is_empty());
        assert!(found("no links here").is_empty());
    }
}
