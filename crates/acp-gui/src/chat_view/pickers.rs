//! The buttons below the prompt for the model, the effort and the mode, and what their
//! menus share: the sizes, `MenuLook`'s colors and shadows, how a menu comes in, how the
//! keys move through a menu, and how a setting's "default" choice can stand for another.

mod effort;
mod mode;
pub(crate) mod model_panel;

use std::rc::Rc;

use acp_core::{Harness, SessionKey, Setting, SettingChoice, SettingKind, SettingValue};
use gpui_kit::base::actions::{Confirm, SelectDown, SelectUp};
use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::InputGroupButton;
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::config::harness_of;

/// The text of the model panel, the menus and the buttons that open them, and of
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
/// The heading over the effort menu's levels and the mode menu's modes.
const MENU_HEADING_HEIGHT: Pixels = px(24.);
/// A row's padding on either side. A menu's heading starts as far in, and the check at a
/// row's end stops as far from the edge as a switch does.
const ROW_PAD: Pixels = px(6.);
/// The column every row starts with, for what the button below shows before its name
/// (a mode's icon, an effort level's meter), an agent's mark or the magnifier, so that
/// names and the search text all start on one line, 32px into the list. A row ends in a
/// column as wide for its check.
const LEAD_WIDTH: Pixels = px(16.);
const LEAD_ICON_SIZE: Pixels = px(14.);
const LEAD_GAP: Pixels = px(6.);
/// The agent's mark beside its name in the favorites.
const HEADING_ICON_SIZE: Pixels = px(12.);
/// The model panel's size. Its models scroll inside it, so a tab with few models is
/// as tall as one with many.
const PANEL_WIDTH: Pixels = px(320.);
const PANEL_HEIGHT: Pixels = px(380.);
/// How wide the effort menu is, and the narrowest the mode menu gets.
const EFFORT_MENU_WIDTH: Pixels = px(196.);
/// How round the panel and the menus are, and the rows and tabs in them. The rows
/// sit 4px in from the edge, so their corners follow the surface's.
const SURFACE_RADIUS: Pixels = px(10.);
const ROW_RADIUS: Pixels = px(6.);
const SURFACE_PADDING: Pixels = px(4.);
/// How far above its button the panel or a menu opens.
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

/// The colors of the model panel and the menus. In dark mode the surface is a step
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

    /// The surface of the panel or a menu, rounded and lifted off the window.
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

    /// A row of the model panel, which lights up under the pointer. The menus' rows light
    /// up for the keys too (see `MenuParts::row`).
    fn row(&self, id: impl Into<ElementId>) -> Stateful<Div> {
        let hover = self.hover;
        div()
            .id(id)
            .h(LIST_ROW_HEIGHT)
            .flex_none()
            .px(ROW_PAD)
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

/// The column a row starts with, for its icon or mark, and the one it ends with for its
/// check.
fn lead() -> Div {
    div()
        .w(LEAD_WIDTH)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
}

/// The check at the end of the row in use.
fn check() -> Icon {
    Icon::new(IconName::Check).size(LEAD_ICON_SIZE)
}

/// The column a model's or a level's row ends with: the check if it is the one in use.
/// Kept when empty, so the stars before it line up.
fn check_column(current: bool) -> Div {
    lead().when(current, |column| column.child(check()))
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

/// `surface`, the model panel or a menu, coming in as it opens upward: it
/// rises out of the button below it as it fades in. The popover is drawn only while
/// open, so each open plays this afresh.
fn rise_in<E: Styled + IntoElement + 'static>(
    surface: E,
    id: impl Into<ElementId>,
    window: &Window,
    cx: &App,
) -> AnimationElement<E> {
    let motion = cx.theme().motion_tokens();
    let travel = motion.distance_short.to_pixels(window.rem_size());
    let easing = motion.easing_enter.clone();
    let enter = Animation::new(motion.duration_normal).with_easing(move |t| easing.sample(t));
    come_in(surface, id, enter, move |surface, rest| {
        surface.top(travel * rest)
    })
}

/// What a menu below the prompt keeps while it is open: the focus that brings it the
/// arrow keys, and the row they or the pointer last lit. Only one menu is open at a time.
#[derive(Clone)]
pub(crate) struct MenuNav {
    focus: FocusHandle,
    highlighted: Option<usize>,
}

impl MenuNav {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            focus: cx.focus_handle(),
            highlighted: None,
        }
    }
}

/// What a press on a menu's row, or Enter on its lit row, does with the row's index.
type PickRow = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// The pick of a menu whose rows are `choices` of the session's setting `setting`, by id:
/// it sets the setting to the row's choice and closes the menu.
fn pick_choice(
    setting: &str,
    choices: Vec<String>,
    key: SessionKey,
    view: &WeakEntity<ChatView>,
    popover: &Entity<PopoverState>,
) -> PickRow {
    let (setting, view, popover) = (setting.to_string(), view.clone(), popover.clone());
    Rc::new(move |row, window, cx| {
        let Some(choice) = choices.get(row) else {
            return;
        };
        let _ = view.update(cx, |this, cx| {
            let value = SettingValue::Choice(choice.clone());
            this.change_setting(key, setting.clone(), value, cx)
        });
        popover.update(cx, |state, cx| state.dismiss(window, cx));
    })
}

/// The popover of a menu below the prompt, opening upward from its button. It gives the
/// menu the focus, so the keys reach it, and opens with no row lit.
fn menu_popover(id: &'static str, nav: &MenuNav, view: &WeakEntity<ChatView>) -> Popover {
    let view = view.clone();
    Popover::new(id)
        .appearance(false)
        .anchor(Anchor::BottomLeft)
        .offset(MENU_OFFSET)
        .track_focus(&nav.focus)
        .on_open_change(move |open, _, cx| {
            if *open {
                let _ = view.update(cx, |this, cx| this.highlight_menu_row(None, cx));
            }
        })
}

/// What the parts of an open menu below the prompt share: its look, its light, and what
/// a press on a row, or Enter on the lit one, does.
struct MenuParts<'a> {
    look: MenuLook,
    nav: &'a MenuNav,
    view: &'a WeakEntity<ChatView>,
    pick: PickRow,
}

impl MenuParts<'_> {
    /// The menu's surface, named `name` for screen readers. It holds the focus while
    /// open: Up and Down move the light through its `rows` rows, wrapping at either end,
    /// and Enter or Space picks the lit row (see `main`'s key bindings). Esc is the
    /// popover's own. The light goes out as the pointer leaves the menu.
    fn surface(&self, id: &'static str, name: &str, rows: usize) -> Stateful<Div> {
        let (down, up, out, confirm) = (
            self.view.clone(),
            self.view.clone(),
            self.view.clone(),
            self.view.clone(),
        );
        let pick = self.pick.clone();
        self.look
            .surface()
            .id(id)
            .track_focus(&self.nav.focus)
            .key_context("PickerMenu")
            .role(Role::Menu)
            .aria_label(SharedString::from(name.to_string()))
            .p(SURFACE_PADDING)
            .flex()
            .flex_col()
            .whitespace_nowrap()
            .on_action(move |_: &SelectDown, _, cx| {
                let _ = down.update(cx, |this, cx| this.step_menu_highlight(1, rows, cx));
            })
            .on_action(move |_: &SelectUp, _, cx| {
                let _ = up.update(cx, |this, cx| this.step_menu_highlight(-1, rows, cx));
            })
            .on_action(move |_: &Confirm, window, cx| {
                // The light as it is now: a Down pressed since the last frame has moved it.
                let lit = confirm
                    .upgrade()
                    .and_then(|view| view.read(cx).menu_nav.highlighted);
                if let Some(row) = lit {
                    pick(row, window, cx);
                }
            })
            .on_hover(move |hovered, _, cx| {
                if !hovered {
                    let _ = out.update(cx, |this, cx| this.highlight_menu_row(None, cx));
                }
            })
    }

    /// The menu's `row`th row, a choice of the setting, `current` if it is the one in
    /// use. The keys and the pointer share one light: moving the pointer over a row
    /// lights it, and the keys go on from there. A press picks the row.
    fn row(&self, id: impl Into<ElementId>, row: usize, current: bool) -> Stateful<Div> {
        let (view, pick) = (self.view.clone(), self.pick.clone());
        let lit = self.nav.highlighted == Some(row);
        div()
            .id(id)
            .flex_none()
            .flex()
            .gap(LEAD_GAP)
            .rounded(ROW_RADIUS)
            .role(Role::MenuItemRadio)
            .aria_toggled(if current {
                Toggled::True
            } else {
                Toggled::False
            })
            .when(lit, |item| {
                item.bg(self.look.hover).aria_active_descendant()
            })
            .on_mouse_move(move |_, _, cx| {
                if !lit {
                    let _ = view.update(cx, |this, cx| this.highlight_menu_row(Some(row), cx));
                }
            })
            .on_click(move |_, window, cx| pick(row, window, cx))
    }

    /// The small heading at the top of the menu, the setting's name, at the rows' lead
    /// column.
    fn heading(&self, name: &str) -> Div {
        div()
            .h(MENU_HEADING_HEIGHT)
            .flex_none()
            .pl(ROW_PAD)
            .flex()
            .items_center()
            .text_size(MENU_HEADING_SIZE)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(self.look.muted)
            .child(SharedString::from(name.to_string()))
    }
}

impl ChatView {
    /// Lights `row` of the open menu, or none.
    fn highlight_menu_row(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        if self.menu_nav.highlighted != row {
            self.menu_nav.highlighted = row;
            cx.notify();
        }
    }

    /// Moves the open menu's light `step` rows down, or up for a negative step, among its
    /// `rows`, wrapping at either end. With no row lit, Down lights the first and Up the
    /// last.
    fn step_menu_highlight(&mut self, step: isize, rows: usize, cx: &mut Context<Self>) {
        if rows == 0 {
            return;
        }
        let next = match self.menu_nav.highlighted {
            Some(row) => (row as isize + step).rem_euclid(rows as isize) as usize,
            None if step > 0 => 0,
            None => rows - 1,
        };
        self.highlight_menu_row(Some(next), cx);
    }
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
