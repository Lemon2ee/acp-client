//! The buttons below the prompt for the model, the effort and the mode, and what their
//! menus share: the sizes, `MenuLook`'s colors and shadows, how a menu comes in, and how a
//! setting's "default" choice can stand for another.

mod effort;
mod mode;
pub(crate) mod model_panel;

use acp_core::{Harness, Setting, SettingChoice, SettingKind};
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::InputGroupButton;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::*;

use crate::config::harness_of;

/// The text of the model panel, the effort menu and the buttons that open them, and of
/// the small headings in them. 13px text on 28px rows is the density of macOS's menus.
const MENU_TEXT_SIZE: Pixels = px(13.);
const MENU_LINE_HEIGHT: Pixels = px(16.);
const MENU_HEADING_SIZE: Pixels = px(11.);
/// How tall a model's or an effort level's row is, a switch's row, and the line naming
/// an agent above its models in the favorites, which gets more room above it after the
/// first agent's.
const LIST_ROW_HEIGHT: Pixels = px(28.);
const SWITCH_ROW_HEIGHT: Pixels = px(30.);
const LIST_HEADER_HEIGHT: Pixels = px(26.);
const LIST_HEADER_GAP: Pixels = px(6.);
/// The heading over the effort menu's levels.
const MENU_HEADING_HEIGHT: Pixels = px(24.);
/// A row's padding. The right is narrower, as the star's button has room of its own.
const ROW_PAD_LEFT: Pixels = px(6.);
const ROW_PAD_RIGHT: Pixels = px(4.);
/// The column every row starts with, for a check, an agent's mark or the magnifier, so
/// that names, headings and the search text all start on one line, 32px into the list.
const LEAD_WIDTH: Pixels = px(16.);
const LEAD_ICON_SIZE: Pixels = px(14.);
const LEAD_GAP: Pixels = px(6.);
/// The agent's mark beside its name in the favorites.
const HEADING_ICON_SIZE: Pixels = px(12.);
/// The model panel's size. Its models scroll inside it, so a tab with few models is
/// as tall as one with many.
const PANEL_WIDTH: Pixels = px(320.);
const PANEL_HEIGHT: Pixels = px(380.);
/// How wide the effort menu is.
const EFFORT_MENU_WIDTH: Pixels = px(196.);
/// How round the panel and the effort menu are, and the rows and tabs in them. The rows
/// sit 4px in from the edge, so their corners follow the surface's.
const SURFACE_RADIUS: Pixels = px(10.);
const ROW_RADIUS: Pixels = px(6.);
const SURFACE_PADDING: Pixels = px(4.);
/// How far above its button the panel or the effort menu opens.
const MENU_OFFSET: Pixels = px(6.);
/// How wide the panel's column of tabs is, and how big each tab is.
const TAB_COLUMN_WIDTH: Pixels = px(44.);
const TAB_SIZE: Pixels = px(32.);
/// The agent's mark inside its tab.
const TAB_ICON_SIZE: Pixels = px(16.);
/// How tall the search box's row along the top of the panel is.
const SEARCH_ROW_HEIGHT: Pixels = px(40.);
/// The star's button on a model's row.
const STAR_BUTTON_SIZE: Pixels = px(20.);
/// The buttons below the prompt for the model, the effort and the mode: their height,
/// the icon before the name, and the caret after it.
const PICKER_HEIGHT: Pixels = px(28.);
const PICKER_ICON_SIZE: Pixels = px(14.);
const PICKER_CARET_SIZE: Pixels = px(12.);
/// The effort meter's tallest bar, and each bar's width and the gap between bars.
const METER_HEIGHT: Pixels = px(12.);
const METER_BAR_WIDTH: Pixels = px(2.);
/// Groups a model's row with its star, which shows while the row is pointed at.
const MODEL_ROW_GROUP: &str = "model-panel-row";

/// The colors of the model panel and the effort menu. In dark mode the surface is a step
/// lighter than the window (#1f1f1f over #151515): the theme's popover color, #0a0a0a,
/// is darker than the window and made the panel look sunk into it.
#[derive(Clone, Copy)]
struct MenuLook {
    dark: bool,
    surface: Hsla,
    /// Behind the panel's column of tabs.
    rail: Hsla,
    /// Between the panel's search box, its list and its switches.
    line: Hsla,
    /// Under the pointer.
    hover: Hsla,
    /// The tab on show, a button whose menu is open, the star's button being pressed.
    pressed: Hsla,
    foreground: Hsla,
    muted: Hsla,
    /// A starred model's star.
    star: Hsla,
    /// The effort meter's bars above the level.
    meter_off: Hsla,
}

impl MenuLook {
    fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let foreground = theme.foreground;
        if theme.is_dark() {
            Self {
                dark: true,
                surface: rgb(0x1f1f1f).into(),
                rail: rgb(0x1a1a1a).into(),
                line: white().opacity(0.08),
                hover: white().opacity(0.07),
                pressed: white().opacity(0.11),
                foreground,
                muted: theme.muted_foreground,
                star: rgb(0xF5B83D).into(),
                meter_off: white().opacity(0.18),
            }
        } else {
            Self {
                dark: false,
                surface: white(),
                rail: rgb(0xf6f6f6).into(),
                line: black().opacity(0.08),
                hover: black().opacity(0.05),
                pressed: black().opacity(0.08),
                foreground,
                // A shade darker than the theme's #737373, which is under 4.5:1 on the
                // tab column's gray.
                muted: rgb(0x6b6b6b).into(),
                // Darker than the dark mode's amber, to stay 3:1 on white.
                star: rgb(0xB07800).into(),
                meter_off: black().opacity(0.16),
            }
        }
    }

    /// A 1px ring around the surface and two soft shadows below it. GPUI's blur is the
    /// gaussian's deviation, half of CSS's blur radius, so 18 and 6 here are CSS's 36
    /// and 12.
    fn shadow(&self) -> Vec<BoxShadow> {
        let ink = |alpha| black().opacity(alpha);
        let (ring, far, near, far_spread, near_spread) = if self.dark {
            (0.6, 0.6, 0.4, -8., -2.)
        } else {
            (0.09, 0.2, 0.1, -10., -4.)
        };
        vec![
            BoxShadow::new(px(0.), px(0.), ink(ring)).spread_radius(px(1.)),
            BoxShadow::new(px(0.), px(16.), ink(far))
                .blur_radius(px(18.))
                .spread_radius(px(far_spread)),
            BoxShadow::new(px(0.), px(4.), ink(near))
                .blur_radius(px(6.))
                .spread_radius(px(near_spread)),
        ]
    }

    /// The surface of the panel or the effort menu, rounded and lifted off the window.
    fn surface(&self) -> Div {
        div()
            .relative()
            .bg(self.surface)
            .text_color(self.foreground)
            .text_size(MENU_TEXT_SIZE)
            .line_height(MENU_LINE_HEIGHT)
            .rounded(SURFACE_RADIUS)
            .shadow(self.shadow())
    }

    /// In dark mode, a hairline just inside the surface's edge, as macOS's menus have.
    /// It goes last, over the surface's content, so the tab column's gray can't hide it.
    fn edge(&self) -> Option<Div> {
        self.dark.then(|| {
            div()
                .absolute()
                .inset_0()
                .rounded(SURFACE_RADIUS)
                .border_1()
                .border_color(white().opacity(0.07))
        })
    }

    /// A row of the panel or the effort menu, which lights up under the pointer.
    fn row(&self, id: impl Into<ElementId>) -> Stateful<Div> {
        let hover = self.hover;
        div()
            .id(id)
            .h(LIST_ROW_HEIGHT)
            .flex_none()
            .pl(ROW_PAD_LEFT)
            .pr(ROW_PAD_RIGHT)
            .flex()
            .items_center()
            .gap(LEAD_GAP)
            .rounded(ROW_RADIUS)
            .hover(move |style| style.bg(hover))
    }

    /// The ghost look of the buttons below the prompt: a gray under the pointer and a
    /// stronger one while the button's menu is open.
    fn button(&self, cx: &App) -> ButtonCustomVariant {
        ButtonCustomVariant::new(cx)
            .foreground(self.foreground)
            .hover(self.hover)
            .active(self.pressed)
    }
}

/// The column a row starts with, for its check or mark.
fn lead() -> Div {
    div()
        .w(LEAD_WIDTH)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

/// A check in a row's `lead`, before the model or level in use.
fn check() -> Icon {
    Icon::new(IconName::Check).size(LEAD_ICON_SIZE)
}

/// The harnesses' marks, for the panel's tabs.
const CLAUDE_LOGO: &[u8] = include_bytes!("../../assets/claude.svg");
const OPENAI_LOGO: &[u8] = include_bytes!("../../assets/openai.svg");
/// The orange Claude's mark is drawn in, in both light and dark mode.
const CLAUDE_ORANGE: u32 = 0xD97757;

/// The agent's mark for its tab: its harness's, or a terminal for a command.
pub(crate) fn agent_logo(name: &str) -> Icon {
    match harness_of(name) {
        Some(Harness::Claude) => Icon::empty()
            .data(CLAUDE_LOGO)
            .text_color(rgb(CLAUDE_ORANGE)),
        Some(Harness::Codex) => Icon::empty().data(OPENAI_LOGO),
        None => Icon::new(IconName::SquareTerminal),
    }
}

/// The kinds of setting whose "default" choice can stand for another: Claude's adapter
/// lists the model and the thought level that way. A mode named "default" is a mode.
const STAND_IN_KINDS: [SettingKind; 2] = [SettingKind::Model, SettingKind::ThoughtLevel];

/// The setting's "default" choice and the choice it stands for, when it stands for one:
/// its description names that choice. A "default" that names none is a choice of its
/// own, and is shown and listed like the others.
fn stand_in(setting: &Setting) -> Option<(&SettingChoice, &SettingChoice)> {
    if !STAND_IN_KINDS.contains(&setting.kind) {
        return None;
    }
    let default = setting
        .choices
        .iter()
        .find(|choice| choice.id == "default")?;
    let stands_for = default.description.as_deref()?;
    let choice = setting
        .choices
        .iter()
        .find(|choice| choice.id != "default" && choice.name == stands_for)?;
    Some((default, choice))
}

/// The setting's choices as they are listed: all of them but a "default" that stands
/// for one of the others.
fn listed_choices(setting: &Setting) -> impl Iterator<Item = &SettingChoice> {
    let hidden = stand_in(setting).map(|(default, _)| default.id.as_str());
    setting
        .choices
        .iter()
        .filter(move |choice| Some(choice.id.as_str()) != hidden)
}

/// The choice a setting shows as its value: the one it is set to, or the one its
/// "default" stands for when it is set to that.
fn shown_choice(setting: &Setting) -> Option<&SettingChoice> {
    let chosen = setting.chosen()?;
    match stand_in(setting) {
        Some((default, stands_for)) if default.id == chosen.id => Some(stands_for),
        _ => Some(chosen),
    }
}

/// `surface`, a panel with the popover style, fading in on `animation` as it moves the
/// last of the way into place: `place` puts it `rest` of the way back, from 1 down to 0.
/// GPUI has no transform for a whole panel, so it moves by its offsets, and fades each
/// thing it paints alone rather than the panel as one, so a half-faded panel would show
/// its own shadow through it. The shadow comes in by the cube of the fade instead, out
/// of sight until the panel covers it, as the component library's dropdowns do. With
/// reduced motion on, GPUI draws the panel in place at once.
fn come_in<E: Styled + IntoElement + 'static>(
    surface: E,
    id: impl Into<ElementId>,
    animation: Animation,
    place: impl Fn(E, f32) -> E + 'static,
) -> AnimationElement<E> {
    surface.with_animation(id, animation, move |mut surface, delta| {
        for shadow in surface.style().box_shadow.iter_mut().flatten() {
            shadow.color.a *= delta.powi(3);
        }
        place(surface, 1. - delta).opacity(delta)
    })
}

/// `surface`, the model panel or the effort menu, coming in as it opens upward: it
/// rises out of the button below it as it fades in. The popover is drawn only while
/// open, so each open plays this afresh.
fn rise_in(
    surface: Div,
    id: impl Into<ElementId>,
    window: &Window,
    cx: &App,
) -> AnimationElement<Div> {
    let motion = cx.theme().motion_tokens();
    let travel = motion.distance_short.to_pixels(window.rem_size());
    let easing = motion.easing_enter.clone();
    let enter = Animation::new(motion.duration_normal).with_easing(move |t| easing.sample(t));
    come_in(surface, id, enter, move |surface, rest| {
        surface.top(travel * rest)
    })
}

/// A button below the prompt: an icon, a name and a caret, in 13px text on a 28px
/// row, a step smaller than the component library's own, to match the menus they open.
/// It grays under the pointer and stays gray while its menu is open.
fn picker_button(
    id: &'static str,
    icon: Option<AnyElement>,
    name: String,
    accessibility_label: String,
    cx: &App,
) -> InputGroupButton {
    let look = MenuLook::new(cx);
    InputGroupButton::new(id)
        .custom(look.button(cx))
        .small()
        .accessibility_label(accessibility_label)
        .h(PICKER_HEIGHT)
        .pl(px(10.))
        .pr(px(8.))
        .rounded(ROW_RADIUS)
        .children(icon)
        .child(
            // The button's own 14px text is set on the row the name sits in.
            div()
                .min_w(px(0.))
                .text_size(MENU_TEXT_SIZE)
                .line_height(MENU_LINE_HEIGHT)
                .text_ellipsis()
                .child(SharedString::from(name)),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .size(PICKER_CARET_SIZE)
                .text_color(look.muted),
        )
}
