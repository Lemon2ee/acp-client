//! What is pasted into a note besides text: files copied in Finder, kept by their paths,
//! and images copied as pictures, saved in the app's folder. They sit under the note's
//! text as tiles, each with a × that takes it off while the note is in front, and stay
//! with the note on its card.

use gpui_kit::assets::IconName;
use gpui_kit::base::{Transition, transition};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::config::{StuffAttachment, forget_attachment, save_pasted_image};

/// Tiles are 48px squares, or as tall for a file, 6px apart and as round.
const TILE_SIZE: Pixels = px(48.);
const TILE_GAP: Pixels = px(6.);
const TILE_RADIUS: Pixels = px(6.);
/// A file's tile: its icon 10px in, then its name over its size, ending 12px short of
/// the tile's end.
const FILE_PAD_LEFT: Pixels = px(10.);
const FILE_PAD_RIGHT: Pixels = px(12.);
const FILE_GAP: Pixels = px(8.);
const FILE_ICON_SIZE: Pixels = px(16.);
const NAME_TEXT_SIZE: Pixels = px(12.);
const NAME_LINE_HEIGHT: Pixels = px(16.);
const SIZE_TEXT_SIZE: Pixels = px(11.);
const SIZE_LINE_HEIGHT: Pixels = px(14.);
/// The tiles start 8px below the text of the note or the card.
pub(super) const STRIP_GAP: Pixels = px(8.);
/// The × that takes a tile off the note: 18px across, 6px out past the tile's top right
/// corner.
const REMOVE_SIZE: Pixels = px(18.);
const REMOVE_OVERHANG: Pixels = px(6.);
const REMOVE_ICON_SIZE: Pixels = px(10.);

/// Which tile on the note the pointer is over, and which tile's ×: the × reaches out
/// past its tile, so the pointer can be on it and off the tile.
#[derive(Default)]
pub(super) struct TileHover {
    tile: Option<usize>,
    remove: Option<usize>,
}

impl TileHover {
    fn shows_remove(&self, ix: usize) -> bool {
        self.tile == Some(ix) || self.remove == Some(ix)
    }
}

/// How the tiles look.
pub(super) struct TileLook {
    background: Hsla,
    ring: Hsla,
    muted: Hsla,
    remove_background: Hsla,
    remove_shadow: Vec<BoxShadow>,
}

impl TileLook {
    pub(super) fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let ink = |alpha| black().opacity(alpha);
        let (background, ring, remove_background, remove_shadow) = if theme.is_dark() {
            (
                rgb(0x262626).into(),
                white().opacity(0.04),
                rgb(0x3a3a3a).into(),
                vec![BoxShadow::new(px(0.), px(0.), ink(0.5)).spread_radius(px(1.))],
            )
        } else {
            (
                rgb(0xf2f2f2).into(),
                ink(0.05),
                white(),
                // GPUI's blur is half of CSS's, so 1 here is CSS's 2.
                vec![
                    BoxShadow::new(px(0.), px(0.), ink(0.12)).spread_radius(px(1.)),
                    BoxShadow::new(px(0.), px(1.), ink(0.1)).blur_radius(px(1.)),
                ],
            )
        };
        Self {
            background,
            ring,
            muted: theme.muted_foreground,
            remove_background,
            remove_shadow,
        }
    }

    /// The tiles of `attachments`, in rows that wrap.
    pub(super) fn strip(&self, attachments: &[StuffAttachment]) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap(TILE_GAP)
            .children(attachments.iter().map(|attachment| self.tile(attachment)))
    }

    /// An image as a square it fills, or a file as its icon, name and size.
    fn tile(&self, attachment: &StuffAttachment) -> Div {
        let tile = div()
            .flex_none()
            .h(TILE_SIZE)
            .rounded(TILE_RADIUS)
            .bg(self.background)
            .shadow(vec![
                BoxShadow::new(px(0.), px(0.), self.ring).spread_radius(px(1.)),
            ]);
        let muted = self.muted;
        match attachment {
            StuffAttachment::Image { path, .. } => tile.w(TILE_SIZE).child(
                img(path.clone())
                    .size_full()
                    .rounded(TILE_RADIUS)
                    .object_fit(ObjectFit::Cover)
                    // An image moved or deleted since it was pasted.
                    .with_fallback(move || {
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::File)
                                    .size(FILE_ICON_SIZE)
                                    .text_color(muted),
                            )
                            .into_any_element()
                    }),
            ),
            StuffAttachment::File { size, .. } => {
                let (icon, about) = match size {
                    Some(size) => (IconName::File, file_size(*size)),
                    None => (IconName::Folder, "Folder".to_string()),
                };
                tile.max_w_full()
                    .min_w_0()
                    .pl(FILE_PAD_LEFT)
                    .pr(FILE_PAD_RIGHT)
                    .flex()
                    .items_center()
                    .gap(FILE_GAP)
                    .child(
                        Icon::new(icon)
                            .size(FILE_ICON_SIZE)
                            .text_color(muted)
                            .flex_none(),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(NAME_TEXT_SIZE)
                                    .line_height(NAME_LINE_HEIGHT)
                                    .truncate()
                                    .child(attachment.name()),
                            )
                            .child(
                                div()
                                    .text_size(SIZE_TEXT_SIZE)
                                    .line_height(SIZE_LINE_HEIGHT)
                                    .text_color(muted)
                                    .child(about),
                            ),
                    )
            }
        }
    }
}

impl ChatView {
    /// Takes what the clipboard holds besides text into the note: files copied in
    /// Finder and an image copied as a picture. Text, and the names Finder copies with
    /// its files, are left to the field.
    pub(super) fn paste_into_note(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let mut added = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::ExternalPaths(paths) => added.extend(
                    paths
                        .paths()
                        .iter()
                        .map(|path| StuffAttachment::copied(path)),
                ),
                ClipboardEntry::Image(image) => match save_pasted_image(image) {
                    Ok(attachment) => added.push(attachment),
                    Err(err) => eprintln!("couldn't keep the pasted image: {err}"),
                },
                ClipboardEntry::String(_) => {}
            }
        }
        if added.is_empty() {
            return false;
        }
        self.stuff.pending.extend(added);
        cx.notify();
        true
    }

    /// Takes the tile at `ix` off the note, and with it the image the app saved for it.
    fn remove_pending(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.stuff.pending.len() {
            forget_attachment(&self.stuff.pending.remove(ix));
            self.stuff.tile_hover = TileHover::default();
            cx.notify();
        }
    }

    /// The tiles pasted into the note, each with a × that fades in over the fast
    /// duration while the pointer is over the tile or the × itself.
    pub(super) fn note_attachments(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let look = TileLook::new(cx);
        let motion = cx.theme().motion_tokens();
        let shown = Transition::new(motion.duration_fast).easing(motion.easing_enter.clone());
        let tiles = self
            .stuff
            .pending
            .iter()
            .enumerate()
            .map(|(ix, attachment)| {
                let target = if self.stuff.tile_hover.shows_remove(ix) {
                    1.
                } else {
                    0.
                };
                let shown = transition(
                    ElementId::from(("stuff-remove", ix)),
                    target,
                    shown.clone(),
                    window,
                    cx,
                );
                let remove = div()
                    .id(("stuff-remove", ix))
                    .role(Role::Button)
                    .aria_label(format!("Remove {}", attachment.name()))
                    .absolute()
                    .top(-REMOVE_OVERHANG)
                    .right(-REMOVE_OVERHANG)
                    .size(REMOVE_SIZE)
                    .rounded(REMOVE_SIZE / 2.)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(look.remove_background)
                    .shadow(look.remove_shadow.clone())
                    .opacity(shown)
                    .cursor_pointer()
                    .child(Icon::new(IconName::Close).size(REMOVE_ICON_SIZE))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        let hover = &mut this.stuff.tile_hover;
                        if *hovered {
                            hover.remove = Some(ix);
                        } else if hover.remove == Some(ix) {
                            hover.remove = None;
                        }
                        cx.notify();
                    }))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.remove_pending(ix, cx)),
                    );
                div()
                    .id(("stuff-tile", ix))
                    .relative()
                    .max_w_full()
                    .min_w_0()
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        let hover = &mut this.stuff.tile_hover;
                        if *hovered {
                            hover.tile = Some(ix);
                        } else if hover.tile == Some(ix) {
                            hover.tile = None;
                        }
                        cx.notify();
                    }))
                    .child(look.tile(attachment))
                    .child(remove)
            });
        div().flex().flex_wrap().gap(TILE_GAP).children(tiles)
    }
}

/// A file's size as Finder gives it, in thousands: bytes, whole kilobytes, then
/// megabytes and gigabytes to one place. Each unit gives way to the next where it would
/// round up to a thousand.
fn file_size(bytes: u64) -> String {
    const KB: f64 = 1000.;
    const MB: f64 = KB * KB;
    const GB: f64 = MB * KB;
    let size = bytes as f64;
    if bytes < 1000 {
        format!("{bytes} B")
    } else if size < MB - KB / 2. {
        format!("{} KB", (size / KB).round())
    } else if size < GB - MB / 20. {
        format!("{:.1} MB", size / MB)
    } else {
        format!("{:.1} GB", size / GB)
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::file_size;

    #[test]
    fn gives_sizes_as_finder_does() {
        assert_eq!(file_size(0), "0 B");
        assert_eq!(file_size(999), "999 B");
        assert_eq!(file_size(1000), "1 KB");
        assert_eq!(file_size(48_200), "48 KB");
        assert_eq!(file_size(999_400), "999 KB");
        assert_eq!(file_size(999_600), "1.0 MB");
        assert_eq!(file_size(1_234_567), "1.2 MB");
        assert_eq!(file_size(999_960_000), "1.0 GB");
        assert_eq!(file_size(999_940_000), "999.9 MB");
        assert_eq!(file_size(3_460_000_000), "3.5 GB");
    }
}
