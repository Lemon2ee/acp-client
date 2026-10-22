//! The effort menu: how hard the model thinks, each level with its meter.

use acp_core::{SessionKey, Setting, SettingChoice};
use gpui_kit::component::input::InputGroupButton;
use gpui_kit::component::popover::PopoverState;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::{
    EFFORT_MENU_WIDTH, LIST_ROW_HEIGHT, METER_BAR_WIDTH, METER_HEIGHT, MenuLook, MenuNav,
    MenuParts, ROW_PAD, check_column, lead, listed_choices, menu_popover, pick_choice,
    picker_button, rise_in, shown_choice,
};

/// A meter of how hard the model thinks: one bar for each of `levels` levels, rising to
/// the right, with those up to `level` in `on` and the rest in `off`.
fn effort_meter(level: usize, levels: usize, on: Hsla, off: Hsla) -> Div {
    div()
        .flex_none()
        .h(METER_HEIGHT)
        .flex()
        .items_end()
        .gap(METER_BAR_WIDTH)
        .children((0..levels).map(|bar| {
            // The last bar is full height and each one before it 2px shorter.
            let height = (METER_HEIGHT - px(2.) * (levels - 1 - bar) as f32).max(px(2.));
            div()
                .w(METER_BAR_WIDTH)
                .h(height)
                .rounded(px(1.))
                .bg(if bar <= level { on } else { off })
        }))
}

/// Where `choice` stands among the setting's levels, and how many there are, for the
/// effort meter. A "default" level has no place of its own, so it gets no meter, and
/// neither does a setting of fewer than two levels.
fn effort_level(setting: &Setting, choice: &SettingChoice) -> Option<(usize, usize)> {
    let levels: Vec<&SettingChoice> = listed_choices(setting)
        .filter(|level| level.id != "default")
        .collect();
    let at = levels.iter().position(|level| level.id == choice.id)?;
    (levels.len() >= 2).then_some((at, levels.len()))
}

/// A level's row in the effort menu: its meter in the lead column, as the button shows
/// it before the name, the name, and a check at the end of the level in use. The meter
/// is muted but for the level in use. A "default" level has no meter of its own.
fn level_row(
    menu: &MenuParts,
    setting: &Setting,
    choice: &SettingChoice,
    row: usize,
    current: bool,
) -> Stateful<Div> {
    let look = &menu.look;
    let ink = if current { look.foreground } else { look.muted };
    // Five levels' meter is 18px wide: centered, it runs a pixel past the column on
    // either side.
    let meter = effort_level(setting, choice)
        .map(|(level, levels)| effort_meter(level, levels, ink, look.meter_off));
    menu.row(
        ElementId::Name(format!("effort-{}", choice.id).into()),
        row,
        current,
    )
    .aria_label(SharedString::from(choice.name.clone()))
    .h(LIST_ROW_HEIGHT)
    .px(ROW_PAD)
    .items_center()
    .child(lead().children(meter))
    .child(
        div()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .text_ellipsis()
            .child(SharedString::from(choice.name.clone())),
    )
    .child(check_column(current))
}

/// The effort menu: the setting's name over its levels. A press or Enter sets the level
/// and closes the menu.
fn effort_menu(
    setting: &Setting,
    nav: &MenuNav,
    view: &WeakEntity<ChatView>,
    key: SessionKey,
    popover: &Entity<PopoverState>,
    window: &Window,
    cx: &App,
) -> AnimationElement<Stateful<Div>> {
    let levels: Vec<&SettingChoice> = listed_choices(setting).collect();
    let values = levels.iter().map(|choice| choice.id.clone());
    let pick = pick_choice(&setting.id, values.collect(), key, view, popover);
    let menu = MenuParts {
        look: MenuLook::new(cx),
        nav,
        view,
        pick,
    };
    let current = shown_choice(setting).map(|choice| choice.id.as_str());
    let rows = levels.iter().enumerate().map(|(row, choice)| {
        level_row(
            &menu,
            setting,
            choice,
            row,
            current == Some(choice.id.as_str()),
        )
    });
    let surface = menu
        .surface("effort-menu", &setting.name, levels.len())
        .w(EFFORT_MENU_WIDTH)
        .child(menu.heading(&setting.name))
        .children(rows)
        .children(menu.look.edge());
    rise_in(surface, "effort-menu-in", window, cx)
}

/// The button for how hard the model thinks, named for the level, with its meter.
fn effort_button(setting: &Setting, cx: &App) -> InputGroupButton {
    let look = MenuLook::new(cx);
    let shown = shown_choice(setting);
    let name = shown.map_or_else(|| setting.name.clone(), |choice| choice.name.clone());
    let meter = shown
        .and_then(|choice| effort_level(setting, choice))
        .map(|(level, levels)| {
            effort_meter(level, levels, look.foreground, look.meter_off).into_any_element()
        });
    let label = format!("{}: {name}", setting.name);
    picker_button("effort-picker", meter, name, label, cx)
}

impl ChatView {
    /// The button for how hard the model thinks, and a menu of the levels that opens
    /// upward. A "default" level that stands for one of the others is left out. The menu
    /// is a popover of its own rather than the component library's menu, whose surface
    /// and rows can't take the model panel's look.
    pub(crate) fn render_effort_picker(
        &self,
        key: SessionKey,
        setting: &Setting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let setting = setting.clone();
        let nav = self.menu_nav.clone();
        menu_popover("effort-menu", &nav, &view)
            .trigger(effort_button(&setting, cx).cursor_pointer())
            .content(move |_, window, cx| {
                let popover = cx.entity();
                effort_menu(&setting, &nav, &view, key, &popover, window, cx)
            })
            .into_any_element()
    }
}
