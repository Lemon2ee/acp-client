//! The mode menu: the modes the agent offers, each with its icon and what it does.

use acp_core::{SessionKey, Setting, SettingChoice, SettingValue};
use gpui_kit::component::input::InputGroupButton;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ChatView;
use crate::pickers::{PICKER_ICON_SIZE, picker_button};

/// How wide a mode's name and description may run, each on one line. Codex's longest
/// description is about 500px; anything past this is cut short with an ellipsis.
const MODE_TEXT_MAX_WIDTH: Pixels = px(560.);

/// A row of the mode menu: the mode's icon and name, what it does below the name, and
/// a check after the current mode. The icon and the check sit level with the name.
fn mode_item(choice: &SettingChoice, current: bool) -> PopupMenuItem {
    let icon = mode_icon(&choice.id);
    let name = SharedString::from(choice.name.clone());
    let description = choice.description.clone().map(SharedString::from);
    PopupMenuItem::element(move |_, cx| {
        let muted = cx.theme().muted_foreground;
        // A box as tall as the name's line.
        let line = || div().flex_none().h(px(20.)).flex().items_center();
        h_flex()
            .flex_1()
            .items_start()
            .gap_2()
            .py_1p5()
            .child(line().child(Icon::new(icon).size(px(16.))))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    // The cap has to be here, not on the menu: asked for its narrowest
                    // width, gpui's text answers with its whole line, so a row under a
                    // narrower menu keeps that width and runs past the menu's edge.
                    .max_w(MODE_TEXT_MAX_WIDTH)
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(div().line_height(px(20.)).child(name.clone()))
                    .children(
                        description.clone().map(|description| {
                            div().text_xs().text_color(muted).child(description)
                        }),
                    ),
            )
            // Kept when empty, so every row leaves its text the same room.
            .child(line().w(px(12.)).ml_1().when(current, |line| {
                line.child(Icon::new(IconName::Check).xsmall())
            }))
    })
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
    /// upward, each with its icon and what the agent says it does.
    pub(crate) fn render_mode_picker(
        &self,
        key: SessionKey,
        setting: &Setting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let setting = setting.clone();
        mode_button(&setting, cx)
            .cursor_pointer()
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, _| {
                // As wide as its widest row; `mode_item` caps how wide a row gets.
                // Left unset, gpui-component's own cap is 500px, short of a long row.
                let menu = menu.min_w(px(260.)).max_w(window.viewport_size().width);
                setting.choices.iter().fold(menu, |menu, choice| {
                    let current = setting.value == SettingValue::Choice(choice.id.clone());
                    let (view, id, value) = (view.clone(), setting.id.clone(), choice.id.clone());
                    menu.item(mode_item(choice, current).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.change_setting(
                                key,
                                id.clone(),
                                SettingValue::Choice(value.clone()),
                                cx,
                            )
                        });
                    }))
                })
            })
            .into_any_element()
    }
}
