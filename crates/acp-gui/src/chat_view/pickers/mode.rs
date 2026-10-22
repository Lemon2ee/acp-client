//! The mode menu: the modes the agent offers, each with its icon and what it does.

use acp_core::{SessionKey, Setting, SettingChoice};
use gpui_kit::component::Icon;
use gpui_kit::component::input::InputGroupButton;
use gpui_kit::component::popover::PopoverState;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::{
    EFFORT_MENU_WIDTH, LEAD_ICON_SIZE, MENU_LINE_HEIGHT, MenuLook, MenuNav, MenuParts,
    PICKER_ICON_SIZE, ROW_PAD_LEFT, check, lead, menu_popover, pick_choice, picker_button, rise_in,
};

/// How wide a mode's name and description may run, each on one line. Codex's longest
/// description is about 500px; anything past this is cut short with an ellipsis.
const MODE_TEXT_MAX_WIDTH: Pixels = px(560.);
/// The room between a mode's text and the check after the mode in use, on top of the
/// row's own gap, so a long description never runs up against the check.
const MODE_CHECK_GAP: Pixels = px(8.);

/// A row of the mode menu: the mode's icon, its name with what it does below, and a
/// check at the end of the mode in use. The icon and the check sit level with the name.
/// The icon is muted but for the mode in use, as the button before it shows that one.
fn mode_row(menu: &MenuParts, choice: &SettingChoice, row: usize, current: bool) -> Stateful<Div> {
    let look = &menu.look;
    // A column as tall as the name's line.
    let line = || lead().h(MENU_LINE_HEIGHT);
    menu.row(
        ElementId::Name(format!("mode-{}", choice.id).into()),
        row,
        current,
    )
    .aria_label(SharedString::from(choice.name.clone()))
    .when_some(choice.description.clone(), |row, description| {
        row.aria_description(SharedString::from(description))
    })
    .p(ROW_PAD_LEFT)
    .items_start()
    .child(
        line().child(
            Icon::new(mode_icon(&choice.id))
                .size(LEAD_ICON_SIZE)
                .text_color(if current { look.foreground } else { look.muted }),
        ),
    )
    .child(
        div()
            .flex_1()
            .min_w(px(0.))
            // The cap has to be here, not on the menu: asked for its narrowest width,
            // gpui's text answers with its whole line, so a row under a narrower menu
            // keeps that width and runs past the menu's edge.
            .max_w(MODE_TEXT_MAX_WIDTH)
            .flex()
            .flex_col()
            .text_ellipsis()
            .child(SharedString::from(choice.name.clone()))
            .children(choice.description.clone().map(|description| {
                div()
                    .text_xs()
                    .line_height(MENU_LINE_HEIGHT)
                    .text_color(look.muted)
                    .child(SharedString::from(description))
            })),
    )
    // Kept when empty, so every row leaves its text the same room.
    .child(
        line()
            .ml(MODE_CHECK_GAP)
            .when(current, |line| line.child(check())),
    )
}

/// The icon for a mode, by the ids Claude's and Codex's adapters give their modes.
fn mode_icon(id: &str) -> gpui_kit::assets::IconName {
    use gpui_kit::assets::IconName;
    match id {
        // Asks before every change.
        "default" => IconName::Hand,
        "acceptEdits" => IconName::FilePen,
        "plan" => IconName::ListChecks,
        "read-only" => IconName::Eye,
        "workspace-write" => IconName::FolderPen,
        // Asks only about what the agent judges unsafe.
        "auto" | "agent" => IconName::ShieldCheck,
        // Never asks.
        "bypassPermissions" | "agent-full-access" => IconName::ShieldOff,
        _ => IconName::Shield,
    }
}

/// The mode menu: the setting's name over the modes, as wide as its widest row. A press
/// or Enter sets the mode and closes the menu.
fn mode_menu(
    setting: &Setting,
    nav: &MenuNav,
    view: &WeakEntity<ChatView>,
    key: SessionKey,
    popover: &Entity<PopoverState>,
    window: &Window,
    cx: &App,
) -> AnimationElement<Stateful<Div>> {
    let modes = setting.choices.iter().map(|choice| choice.id.clone());
    let pick = pick_choice(&setting.id, modes.collect(), key, view, popover);
    let menu = MenuParts {
        look: MenuLook::new(cx),
        nav,
        view,
        pick,
    };
    let current = setting.chosen().map(|choice| choice.id.as_str());
    let rows = setting
        .choices
        .iter()
        .enumerate()
        .map(|(row, choice)| mode_row(&menu, choice, row, current == Some(choice.id.as_str())));
    let surface = menu
        .surface("mode-menu", &setting.name, setting.choices.len())
        // No narrower than the effort menu beside it; `mode_row` caps how wide a row gets.
        .min_w(EFFORT_MENU_WIDTH)
        .max_w(window.viewport_size().width)
        .child(menu.heading(&setting.name))
        .children(rows)
        .children(menu.look.edge());
    rise_in(surface, "mode-menu-in", window, cx)
}

/// The mode's button, with the mode's icon.
fn mode_button(setting: &Setting, cx: &App) -> InputGroupButton {
    let current = setting.chosen();
    let name = current.map_or_else(|| setting.name.clone(), |choice| choice.name.clone());
    let icon = Icon::new(mode_icon(current.map_or("", |choice| choice.id.as_str())))
        .size(PICKER_ICON_SIZE)
        .into_any_element();
    let label = format!("{}: {name}", setting.name);
    picker_button("mode-picker", Some(icon), name, label, cx)
}

impl ChatView {
    /// The mode's button, with the mode's icon, and a menu of the modes that opens
    /// upward, each with its icon and what the agent says it does. The menu is a popover
    /// of its own, as the effort menu is, rather than the component library's menu,
    /// whose surface and rows can't take the model panel's look.
    pub(crate) fn render_mode_picker(
        &self,
        key: SessionKey,
        setting: &Setting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let setting = setting.clone();
        let nav = self.menu_nav.clone();
        menu_popover("mode-menu", &nav, &view)
            .trigger(mode_button(&setting, cx).cursor_pointer())
            .content(move |_, window, cx| {
                let popover = cx.entity();
                mode_menu(&setting, &nav, &view, key, &popover, window, cx)
            })
            .into_any_element()
    }
}
