//! The effort menu: how hard the model thinks, each level with its meter.

use acp_core::{SessionKey, Setting, SettingChoice, SettingValue};
use gpui_kit::component::input::InputGroupButton;
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chat_view::pickers::{
    EFFORT_MENU_WIDTH, LEAD_GAP, LEAD_WIDTH, MENU_HEADING_HEIGHT, MENU_HEADING_SIZE, MENU_OFFSET,
    METER_BAR_WIDTH, METER_HEIGHT, MenuLook, ROW_PAD_LEFT, SURFACE_PADDING, check, lead,
    listed_choices, picker_button, rise_in, shown_choice,
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

/// The effort menu: the setting's name over its levels, each with a check before the
/// level in use and the meter after it. A press sets the level and closes the menu.
fn effort_menu(
    setting: &Setting,
    view: &WeakEntity<ChatView>,
    key: SessionKey,
    popover: &Entity<PopoverState>,
    window: &Window,
    cx: &App,
) -> AnimationElement<Div> {
    let look = MenuLook::new(cx);
    let current = shown_choice(setting).map(|choice| choice.id.as_str());
    let heading = div()
        .h(MENU_HEADING_HEIGHT)
        .flex_none()
        // Level with the levels' names.
        .pl(ROW_PAD_LEFT + LEAD_WIDTH + LEAD_GAP)
        .flex()
        .items_center()
        .text_size(MENU_HEADING_SIZE)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(look.muted)
        .child(SharedString::from(setting.name.clone()));
    let rows = listed_choices(setting).map(|choice| {
        let (view, popover) = (view.clone(), popover.clone());
        let (id, value) = (setting.id.clone(), choice.id.clone());
        look.row(ElementId::Name(format!("effort-{}", choice.id).into()))
            .child(lead().when(current == Some(choice.id.as_str()), |lead| {
                lead.child(check())
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(choice.name.clone())),
            )
            .children(effort_level(setting, choice).map(|(level, levels)| {
                effort_meter(level, levels, look.muted, look.meter_off).mr(px(4.))
            }))
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    let value = SettingValue::Choice(value.clone());
                    this.change_setting(key, id.clone(), value, cx)
                });
                popover.update(cx, |state, cx| state.dismiss(window, cx));
            })
    });
    let surface = look
        .surface()
        .w(EFFORT_MENU_WIDTH)
        .p(SURFACE_PADDING)
        .flex()
        .flex_col()
        .whitespace_nowrap()
        .child(heading)
        .children(rows)
        .children(look.edge());
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
        Popover::new("effort-menu")
            .appearance(false)
            .anchor(Anchor::BottomLeft)
            .offset(MENU_OFFSET)
            .trigger(effort_button(&setting, cx).cursor_pointer())
            .content(move |_, window, cx| {
                let popover = cx.entity();
                effort_menu(&setting, &view, key, &popover, window, cx)
            })
            .into_any_element()
    }
}
