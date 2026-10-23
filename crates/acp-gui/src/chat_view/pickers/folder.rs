//! The folder menu, opened from the folder button under the prompt box until a session's
//! first prompt: the folders sessions were started in, newest first, and a row that opens
//! the system's folder panel for one not listed.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use acp_core::SessionKey;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::PopoverState;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::folder_line::{FOLDER_BUTTON_MIN, folder_name, tilde};
use crate::chat_view::pickers::{
    EFFORT_MENU_WIDTH, LEAD_ICON_SIZE, LIST_ROW_HEIGHT, MENU_LINE_HEIGHT, MenuLook, MenuParts,
    MenuTrigger, PickRow, ROW_PAD, check, lead, menu_popover, rise_in,
};

/// How wide a folder's name and path may run, each on one line, before an ellipsis.
const FOLDER_TEXT_MAX_WIDTH: Pixels = px(360.);
/// The room between a folder's text and the check after the folder in use, as in the
/// mode menu.
const FOLDER_CHECK_GAP: Pixels = px(8.);
/// Above and below the line between the folders and the row that adds one.
const SEPARATOR_GAP: Pixels = px(4.);

/// The folders the menu lists: the recent ones, after the folder new sessions start in
/// and the session's own where they aren't among them. A folder only becomes recent with
/// a prompt sent in it, so the one new sessions start in would leave the list as soon as
/// the session moved away from it.
fn folder_choices(recent: &[PathBuf], start: &Path, current: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::with_capacity(recent.len() + 2);
    for folder in [start, current] {
        if !recent.iter().chain(&folders).any(|listed| listed == folder) {
            folders.push(folder.to_path_buf());
        }
    }
    folders.extend_from_slice(recent);
    folders
}

/// A folder's row: a folder icon, its name with its path below, and a check at the end
/// of the folder in use. Names alone can't tell two worktrees of one repository apart.
fn folder_row(menu: &MenuParts, folder: &Path, row: usize, current: bool) -> Stateful<Div> {
    let look = &menu.look;
    // A column as tall as the name's line.
    let line = || lead().h(MENU_LINE_HEIGHT);
    let path = SharedString::from(tilde(folder));
    menu.row(
        ElementId::Name(format!("folder-{row}").into()),
        row,
        current,
    )
    .aria_label(path.clone())
    .p(ROW_PAD)
    .items_start()
    .child(
        line().child(
            Icon::new(IconName::Folder)
                .size(LEAD_ICON_SIZE)
                .text_color(if current { look.foreground } else { look.muted }),
        ),
    )
    .child(
        div()
            .flex_1()
            .min_w(px(0.))
            // On the text, not the menu, for the reason `mode_row` gives.
            .max_w(FOLDER_TEXT_MAX_WIDTH)
            .flex()
            .flex_col()
            .child(div().truncate().child(folder_name(folder)))
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .line_height(MENU_LINE_HEIGHT)
                    .text_color(look.muted)
                    .child(path),
            ),
    )
    // Kept when empty, so every row leaves its text the same room.
    .child(
        line()
            .ml(FOLDER_CHECK_GAP)
            .when(current, |line| line.child(check())),
    )
}

/// The last row, which opens the system's folder panel.
fn add_row(menu: &MenuParts, row: usize) -> Stateful<Div> {
    menu.row("folder-add", row, false)
        // A command, not one of the folders to choose between.
        .role(Role::MenuItem)
        .h(LIST_ROW_HEIGHT)
        .px(ROW_PAD)
        .items_center()
        .child(
            lead().child(
                Icon::new(IconName::FolderPlus)
                    .size(LEAD_ICON_SIZE)
                    .text_color(menu.look.muted),
            ),
        )
        .child("Add folder…")
}

/// The pick of the folder menu: a folder's row has the session work there, and the row
/// past them opens the folder panel. Either closes the menu first, so the panel's sheet
/// never comes down over it.
fn pick_folder(
    folders: Vec<PathBuf>,
    key: SessionKey,
    view: &WeakEntity<ChatView>,
    popover: &Entity<PopoverState>,
) -> PickRow {
    let (view, popover) = (view.clone(), popover.clone());
    Rc::new(move |row, window, cx| {
        popover.update(cx, |state, cx| state.dismiss(window, cx));
        let _ = view.update(cx, |this, cx| match folders.get(row) {
            Some(folder) => this.set_folder(key, folder.clone(), window, cx),
            None => this.pick_folder(key, window, cx),
        });
    })
}

/// The folder menu: its heading over the folders, then a line and the row that adds one.
fn folder_menu(
    folders: &[PathBuf],
    current: &Path,
    menu: MenuParts,
    window: &Window,
    cx: &App,
) -> AnimationElement<Stateful<Div>> {
    let rows = folders
        .iter()
        .enumerate()
        .map(|(row, folder)| folder_row(&menu, folder, row, folder == current));
    let surface = menu
        .surface("folder-menu", "Folder", folders.len() + 1)
        // As narrow as the mode menu at least, so a short list doesn't look cramped.
        .min_w(EFFORT_MENU_WIDTH)
        .max_w(window.viewport_size().width)
        .child(menu.heading("Recent folders"))
        .children(rows)
        .child(
            div()
                .flex_none()
                .my(SEPARATOR_GAP)
                .mx(ROW_PAD)
                .h(px(1.))
                .bg(menu.look.line),
        )
        .child(add_row(&menu, folders.len()))
        .children(menu.look.edge());
    rise_in(surface, "folder-menu-in", window, cx)
}

impl ChatView {
    /// `button`, the folder of the session `key`, as the trigger of a menu of the recent
    /// folders that opens upward over the prompt box, as the mode menu does.
    pub(crate) fn render_folder_picker(
        &self,
        key: SessionKey,
        current: &Path,
        button: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let nav = self.menu_nav.clone();
        let folders = folder_choices(&self.recent_folders, &self.cwd, current);
        let current = current.to_path_buf();
        menu_popover("folder-menu", &nav, &view)
            .trigger(MenuTrigger::new(button, cx))
            // The popover's box takes the button's place in the line, so it shrinks as the
            // button did when the line is short of room.
            .trigger_style(
                StyleRefinement::default()
                    .flex()
                    .flex_shrink(1.)
                    .min_w(FOLDER_BUTTON_MIN)
                    .h_full(),
            )
            .content(move |_, window, cx| {
                let popover = cx.entity();
                let menu = MenuParts {
                    look: MenuLook::new(cx),
                    nav: &nav,
                    view: &view,
                    pick: pick_folder(folders.clone(), key, &view, &popover),
                };
                folder_menu(&folders, &current, menu, window, cx)
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::folder_choices;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_start_folder_and_the_one_in_use_are_always_listed_once() {
        let recent = ["/a", "/b"].map(PathBuf::from);
        let (a, b, home) = (Path::new("/a"), Path::new("/b"), Path::new("/home"));
        assert_eq!(folder_choices(&recent, a, b), recent);
        assert_eq!(
            folder_choices(&recent, home, home),
            ["/home", "/a", "/b"].map(PathBuf::from)
        );
        // Moved from the start folder to a recent one, the start folder stays where it was.
        assert_eq!(
            folder_choices(&recent, home, b),
            ["/home", "/a", "/b"].map(PathBuf::from)
        );
        assert_eq!(
            folder_choices(&recent, home, Path::new("/c")),
            ["/home", "/c", "/a", "/b"].map(PathBuf::from)
        );
    }
}
