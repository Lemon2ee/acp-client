//! The Stuff panel, the sidebar's other list: a note in front to write or paste anything
//! into, and the board behind it that each finished note is stuck onto. It belongs to the
//! window, not to a session, so switching sessions leaves the note and the board as they
//! are.

use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Enter, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::MenuLook;
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
/// The strip of tape across a card's top edge, half above it.
const TAPE_WIDTH: Pixels = px(32.);
const TAPE_HEIGHT: Pixels = px(10.);
const TAPE_RADIUS: Pixels = px(2.);
/// The board's dots: 2px across, one every 12px.
const DOT_SPACING: Pixels = px(12.);
const DOT_SIZE: Pixels = px(2.);
const DOT_OFFSET: Pixels = px(1.);
/// The empty board's line, a little way down.
const EMPTY_TOP: Pixels = px(40.);

/// The note and the board, kept for the whole window.
pub(crate) struct StuffState {
    /// Newest first, as on the board.
    notes: Vec<StuffNote>,
    /// The note being written, in front of the board.
    pub(crate) input: Entity<TextareaState>,
    _input_events: Subscription,
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
        }
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
    /// note empties, ready for the next one. An empty note sticks nothing.
    fn stick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.stuff.input.read(cx).value().trim_end().to_string();
        if text.trim().is_empty() {
            return;
        }
        let note = StuffNote {
            id: self.stuff.next_id(),
            text,
        };
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

    /// The Stuff panel: the note in front, the board behind it.
    pub(crate) fn render_stuff(&self, cx: &mut Context<Self>) -> Div {
        let dots = if cx.theme().is_dark() {
            white().opacity(0.06)
        } else {
            black().opacity(0.08)
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .relative()
            .flex()
            .flex_col()
            .child(dot_grid(dots))
            .child(self.render_note(cx))
            .child(self.render_board(cx))
    }

    /// The note in front of the board, on the menus' surface: the field, and a row with
    /// the shortcut and the button that stick it. Cmd+Enter sticks it from the field,
    /// where Enter alone starts a new line.
    fn render_note(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let look = MenuLook::new(cx);
        let muted = cx.theme().muted_foreground;
        let empty = self.stuff.input.read(cx).value().trim().is_empty();
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
        look.surface()
            .id("stuff-note")
            .flex_none()
            .mx(NOTE_INSET)
            .mt(NOTE_INSET)
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
            .child(
                div()
                    .h(STICK_ROW_HEIGHT)
                    .mt(STICK_ROW_GAP)
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
                    .child(stick),
            )
            .children(look.edge())
    }

    /// The board: the stuck notes as cards, newest at the top, scrolling under the note.
    fn render_board(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let dark = theme.is_dark();
        let muted = theme.muted_foreground;
        let (card, tape, link_line) = if dark {
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
        let shadow = card_shadow(dark);
        let cards = self.stuff.notes.iter().map(|note| {
            div()
                .id(("stuff-card", note.id))
                .relative()
                .flex_none()
                .p(CARD_PAD)
                .rounded(CARD_RADIUS)
                .bg(card)
                .shadow(shadow.clone())
                .text_size(TEXT_SIZE)
                .line_height(LINE_HEIGHT)
                .child(
                    div()
                        .absolute()
                        .top(-TAPE_HEIGHT / 2.)
                        .left_1_2()
                        .ml(-TAPE_WIDTH / 2.)
                        .w(TAPE_WIDTH)
                        .h(TAPE_HEIGHT)
                        .rounded(TAPE_RADIUS)
                        .bg(tape),
                )
                .child(card_text(note, link_line))
        });
        div()
            .id("stuff-board")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .pt(BOARD_PAD)
            .px(BOARD_PAD)
            .pb(BOARD_PAD_BOTTOM)
            .flex()
            .flex_col()
            .gap(CARD_GAP)
            .children(cards)
            .when(self.stuff.notes.is_empty(), |board| {
                board.child(
                    div()
                        .pt(EMPTY_TOP)
                        .text_center()
                        .text_xs()
                        .text_color(muted)
                        .child("Stuck notes land here"),
                )
            })
    }
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
