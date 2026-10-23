//! The branch menu, opened from the branch under the prompt box while the agent isn't
//! replying: the local branches the session's folder can switch to, the newest commit
//! first.

use std::rc::Rc;

use acp_core::SessionKey;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::PopoverState;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::{
    EFFORT_MENU_WIDTH, LEAD_ICON_SIZE, LIST_ROW_HEIGHT, MenuLook, MenuParts, MenuTrigger, PickRow,
    ROW_PAD, check_column, lead, menu_popover, rise_in,
};

/// How wide a branch's name may run before an ellipsis.
const BRANCH_TEXT_MAX_WIDTH: Pixels = px(320.);
/// How many rows show before the list scrolls: a repository can have hundreds of branches.
const BRANCH_ROWS_SHOWN: f32 = 12.;

/// A branch's row: a branch icon, its name, and a check at the end of the one checked out.
fn branch_row(menu: &MenuParts, name: &str, row: usize, current: bool) -> Stateful<Div> {
    let look = &menu.look;
    let name = SharedString::from(name.to_string());
    menu.row(
        ElementId::Name(format!("branch-{row}").into()),
        row,
        current,
    )
    .aria_label(name.clone())
    .h(LIST_ROW_HEIGHT)
    .px(ROW_PAD)
    .items_center()
    .child(
        lead().child(
            Icon::new(IconName::GitBranch)
                .size(LEAD_ICON_SIZE)
                .text_color(if current { look.foreground } else { look.muted }),
        ),
    )
    .child(
        div()
            .flex_1()
            .min_w(px(0.))
            // On the text, not the menu, for the reason `mode_row` gives.
            .max_w(BRANCH_TEXT_MAX_WIDTH)
            .truncate()
            .child(name),
    )
    .child(check_column(current))
}

/// The pick of the branch menu: it closes the menu, and switches the folder to the row's
/// branch unless that is the one checked out.
fn pick_branch(
    branches: Vec<String>,
    current: Option<String>,
    key: SessionKey,
    view: &WeakEntity<ChatView>,
    popover: &Entity<PopoverState>,
) -> PickRow {
    let (view, popover) = (view.clone(), popover.clone());
    Rc::new(move |row, window, cx| {
        popover.update(cx, |state, cx| state.dismiss(window, cx));
        let Some(branch) = branches
            .get(row)
            .filter(|branch| Some(*branch) != current.as_ref())
        else {
            return;
        };
        let branch = branch.clone();
        let _ = view.update(cx, |this, cx| this.switch_branch(key, branch, cx));
    })
}

/// The branch menu: its heading over the branches, which scroll past the first dozen.
fn branch_menu(
    branches: &[String],
    current: Option<&str>,
    menu: MenuParts,
    window: &Window,
    cx: &App,
) -> AnimationElement<Stateful<Div>> {
    let rows = branches
        .iter()
        .enumerate()
        .map(|(row, name)| branch_row(&menu, name, row, Some(name.as_str()) == current));
    let surface = menu
        .surface("branch-menu", "Branch", branches.len())
        .min_w(EFFORT_MENU_WIDTH)
        .max_w(window.viewport_size().width)
        .child(menu.heading("Branches"))
        .child(
            div()
                .id("branch-rows")
                .max_h(LIST_ROW_HEIGHT * BRANCH_ROWS_SHOWN)
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows),
        )
        .children(menu.look.edge());
    rise_in(surface, "branch-menu-in", window, cx)
}

impl ChatView {
    /// `trigger`, the branch under the prompt box, as the trigger of a menu of `branches`
    /// that opens upward over the prompt box. `place` lays the popover's box out in the
    /// line where the branch would sit on its own.
    pub(crate) fn render_branch_picker(
        &self,
        key: SessionKey,
        branches: &[String],
        current: Option<&str>,
        trigger: Stateful<Div>,
        place: StyleRefinement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let nav = self.menu_nav.clone();
        let branches = branches.to_vec();
        let current = current.map(str::to_string);
        menu_popover("branch-menu", &nav, &view)
            .trigger(MenuTrigger::new(trigger, cx))
            .trigger_style(place)
            .content(move |_, window, cx| {
                let popover = cx.entity();
                let menu = MenuParts {
                    look: MenuLook::new(cx),
                    nav: &nav,
                    view: &view,
                    pick: pick_branch(branches.clone(), current.clone(), key, &view, &popover),
                };
                branch_menu(&branches, current.as_deref(), menu, window, cx)
            })
            .into_any_element()
    }
}
