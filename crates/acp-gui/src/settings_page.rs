//! The settings, which take the whole window in place of the sidebar and the chat while
//! open: the General, Agents and Keyboard pages, and the rows and groups they are made of.

use std::path::Path;

use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::chrome::{WINDOW_BLUR, as_title_bar};
use crate::config::{AppSettings, Appearance, SETTINGS, config_folder};
use crate::{ChatView, CloseSettings, ResizeSidebar, agent_logo};

/// The widest the settings' column gets: 560 for the settings, and 32 either side.
const SETTINGS_COLUMN_WIDTH: Pixels = px(624.);
/// A settings page's title, and the text of its settings: a setting's name, with what it
/// does below in smaller type, at the sizes macOS's own settings use.
const SETTINGS_TITLE_SIZE: Pixels = px(22.);
const SETTING_TEXT_SIZE: Pixels = px(13.);
const SETTING_NOTE_SIZE: Pixels = px(12.);
/// How tall a setting's row is with its two lines, and a shortcut's with one.
const SETTING_ROW_HEIGHT: Pixels = px(56.);
const SHORTCUT_ROW_HEIGHT: Pixels = px(44.);
/// How far in from a group's edges its rows' text starts, and the lines between them.
const SETTING_ROW_INSET: Pixels = px(16.);
/// The rounded tile behind an agent's mark in the settings' list of agents.
const AGENT_TILE_SIZE: Pixels = px(28.);

/// A page of the settings.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    General,
    Agents,
    Keyboard,
}

impl SettingsPage {
    const ALL: [Self; 3] = [Self::General, Self::Agents, Self::Keyboard];

    fn name(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Agents => "Agents",
            Self::Keyboard => "Keyboard",
        }
    }

    fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::General => IconName::SlidersHorizontal,
            Self::Agents => IconName::Bot,
            Self::Keyboard => IconName::Keyboard,
        }
    }
}

/// What the keyboard page lists, each with its keystroke as `main` binds it, or as the
/// prompt box takes it. None can be changed yet.
const SHORTCUTS: [(&str, &str); 6] = [
    ("Send message", "enter"),
    ("New line", "shift-enter"),
    ("Copy selected text in the chat", "cmd-c"),
    ("Open or leave settings", "cmd-,"),
    ("Leave settings", "escape"),
    ("Quit", "cmd-q"),
];

impl ChatView {
    /// Shows the settings in place of the sidebar and the chat, on the page they were
    /// last left on.
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = true;
        window.focus(&self.settings_focus, cx);
        cx.notify();
    }

    /// Brings back the sidebar and the chat as they were, the prompt box focused.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// The settings over the whole window: a list of their pages where the sidebar goes,
    /// and the page on show where the chat goes. The sessions go on as they were.
    pub(crate) fn render_settings(
        &mut self,
        title_bar: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (sidebar, background) = self.panel_colors(cx);
        let theme = cx.theme();
        let (foreground, sidebar_foreground, muted, hover) = (
            theme.foreground,
            theme.sidebar_foreground,
            theme.muted_foreground,
            theme.secondary,
        );
        let (accent, accent_foreground) = (theme.sidebar_accent, theme.sidebar_accent_foreground);
        let motion = theme.motion_tokens();
        let easing = motion.easing_enter.clone();
        // Each side comes in on its own, as it is drawn anew; the page again for each
        // page picked. Reduced motion draws them at once.
        let fade = Animation::new(motion.duration_fast).with_easing(move |t| easing.sample(t));
        let page = self.settings_page;

        let page_rows = SettingsPage::ALL.map(|item| {
            div()
                .id(("settings-page", item as usize))
                .role(Role::Button)
                .aria_label(item.name())
                .aria_selected(item == page)
                .h(px(32.))
                .flex_none()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .map(|row| {
                    if item == page {
                        row.bg(accent)
                            .text_color(accent_foreground)
                            .font_weight(FontWeight::MEDIUM)
                    } else {
                        row.hover(|style| style.bg(hover))
                    }
                })
                .child(
                    Icon::new(item.icon())
                        .small()
                        .when(item != page, |icon| icon.text_color(muted)),
                )
                .child(item.name())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.settings_page = item;
                    cx.notify();
                }))
        });
        let pages = div()
            .w(self.sidebar_width)
            .flex_none()
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .bg(sidebar)
            .text_color(sidebar_foreground)
            .child(self.sidebar_edge(cx))
            .child(
                // Under the traffic lights; the sidebar button has no use here.
                div()
                    .flex_none()
                    .h(self.title_bar_height)
                    .when(title_bar, as_title_bar),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .child(
                        div()
                            .id("close-settings")
                            .role(Role::Button)
                            .aria_label("Back to app")
                            .h(px(32.))
                            .flex_none()
                            // The arrow starts as far into its icon as the plus of New
                            // session does, so it lines up the same way.
                            .pl(px(10.5))
                            .pr_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .text_sm()
                            .text_color(muted)
                            .cursor_pointer()
                            .hover(|style| style.bg(hover).text_color(sidebar_foreground))
                            .child(Icon::new(IconName::ArrowLeft).small())
                            .child("Back to app")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.close_settings(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .mt_2()
                            .px_3()
                            .pb_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child("Settings"),
                    )
                    .children(page_rows)
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .px_3()
                            .py_1p5()
                            .text_xs()
                            .text_color(muted)
                            .child(concat!("ACP client ", env!("CARGO_PKG_VERSION"))),
                    )
                    .with_animation("settings-pages", fade.clone(), |pages, delta| {
                        pages.opacity(delta)
                    }),
            );

        let content = match page {
            SettingsPage::General => self.general_page(cx),
            SettingsPage::Agents => self.agents_page(cx),
            SettingsPage::Keyboard => keyboard_page(cx),
        };
        let shown = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(background)
            .child(
                div()
                    .flex_none()
                    .h(self.title_bar_height)
                    .when(title_bar, as_title_bar),
            )
            .child(
                div()
                    .id("settings-page")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(SETTINGS_COLUMN_WIDTH)
                            .px_8()
                            .pt_3()
                            .pb_12()
                            .child(
                                div()
                                    .text_size(SETTINGS_TITLE_SIZE)
                                    .line_height(px(28.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(page.name()),
                            )
                            .child(content)
                            .with_animation(
                                ElementId::Name(format!("settings-{}", page.name()).into()),
                                fade,
                                |column, delta| column.opacity(delta),
                            ),
                    ),
            );

        div()
            .size_full()
            .relative()
            .flex()
            .text_color(foreground)
            .track_focus(&self.settings_focus)
            .key_context("Settings")
            .on_action(
                cx.listener(|this, _: &CloseSettings, window, cx| this.close_settings(window, cx)),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeSidebar>, _, cx| {
                    this.resize_sidebar(event, cx)
                }),
            )
            .child(pages)
            .child(shown)
            .into_any_element()
    }

    /// The appearance, and the blur where the window can blur.
    fn general_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let settings = cx.global::<AppSettings>();
        let selected = Appearance::ALL
            .iter()
            .position(|&appearance| appearance == settings.appearance)
            .unwrap_or_default();
        let appearance = TabBar::new("appearance")
            .segmented()
            .selected_index(selected)
            .on_click(|ix: &usize, _, cx| {
                let appearance = Appearance::ALL[*ix];
                AppSettings::change(cx, |settings| settings.appearance = appearance);
            })
            .children(Appearance::ALL.map(|appearance| {
                Tab::new()
                    .label(appearance.name())
                    .icon(Icon::new(appearance.icon()))
            }));
        let mut rows = vec![
            setting_row(
                "Theme",
                Some("Light or dark, or as the system is set."),
                appearance,
                muted,
            )
            .into_any_element(),
        ];
        if WINDOW_BLUR {
            let on = settings.window_blur;
            // The whole row flips the switch, as the model panel's switch rows do.
            rows.push(
                setting_row(
                    "Translucent window",
                    Some("Blur the desktop behind the sidebar and the chat."),
                    Switch::new("window-blur-value").checked(on).tab_stop(false),
                    muted,
                )
                .id("window-blur")
                .role(Role::Switch)
                .aria_label("Translucent window")
                .aria_toggled(on.into())
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    AppSettings::change(cx, |settings| settings.window_blur = !on)
                })
                .into_any_element(),
            );
        }
        let saved_in = config_folder().map(|folder| {
            div()
                .mt_2()
                .mx_0p5()
                .text_size(SETTING_NOTE_SIZE)
                .line_height(px(16.))
                .text_color(muted)
                .child(format!("Saved in {}", shown_path(&folder.join(SETTINGS))))
        });
        v_flex().child(settings_section(Some("Appearance"), rows, border, muted).children(saved_in))
    }

    /// The agents the window started, each with the command it was started with and
    /// whether it is running.
    fn agents_page(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, border, tile) = (theme.muted_foreground, theme.border, theme.secondary);
        let (ok, gone) = (theme.success, theme.danger);
        let rows = self.agents.iter().map(|slot| {
            let (status, color) = if slot.commands.is_closed() {
                ("Stopped", gone)
            } else if slot.connected {
                ("Connected", ok)
            } else {
                ("Starting", muted)
            };
            h_flex()
                .min_h(SETTING_ROW_HEIGHT)
                .px(SETTING_ROW_INSET)
                .py_2p5()
                .gap_4()
                .child(
                    div()
                        .size(AGENT_TILE_SIZE)
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.))
                        .bg(tile)
                        .child(agent_logo(&slot.name)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .gap_0p5()
                        .child(setting_label(slot.name.clone()))
                        .child(
                            setting_note(slot.command.clone(), muted)
                                .font_family(theme.mono_font_family.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1p5()
                        .text_size(SETTING_NOTE_SIZE)
                        .text_color(muted)
                        .child(div().size(px(6.)).rounded_full().bg(color))
                        .child(status),
                )
                .into_any_element()
        });
        v_flex()
            .child(page_lede(
                "The agents this window started, and the command each was started with.",
                muted,
            ))
            .child(
                settings_section(None, rows.collect(), border, muted).child(
                    div()
                        .mt_2()
                        .mx_0p5()
                        .text_size(SETTING_NOTE_SIZE)
                        .line_height(px(16.))
                        .text_color(muted)
                        .child("Sign in from each agent's own CLI. The app never handles your credentials."),
                ),
            )
    }
}

/// The shortcuts, which can't be changed yet.
fn keyboard_page(cx: &App) -> Div {
    let theme = cx.theme();
    let (muted, border) = (theme.muted_foreground, theme.border);
    let shortcut_row = |(name, keystroke): (&'static str, &str)| {
        h_flex()
            .h(SHORTCUT_ROW_HEIGHT)
            .px(SETTING_ROW_INSET)
            .gap_4()
            .child(setting_label(name).flex_1())
            .children(Keystroke::parse(keystroke).ok().map(Kbd::new))
            .into_any_element()
    };
    #[cfg_attr(not(feature = "profile"), expect(unused_mut))]
    let mut page = v_flex()
        .child(page_lede("Shortcuts can't be changed yet.", muted))
        .child(settings_section(
            None,
            SHORTCUTS.into_iter().map(shortcut_row).collect(),
            border,
            muted,
        ));
    #[cfg(feature = "profile")]
    {
        page = page.child(settings_section(
            Some("Profiling"),
            vec![shortcut_row(("Cycle the frame-time overlay", "cmd-alt-f"))],
            border,
            muted,
        ));
    }
    page
}

/// A setting's name, on one line.
fn setting_label(text: impl Into<SharedString>) -> Div {
    div()
        .min_w(px(0.))
        .truncate()
        .text_size(SETTING_TEXT_SIZE)
        .line_height(px(16.))
        .child(text.into())
}

/// What a setting does, below its name, on one line.
fn setting_note(text: impl Into<SharedString>, muted: Hsla) -> Div {
    div()
        .min_w(px(0.))
        .truncate()
        .text_size(SETTING_NOTE_SIZE)
        .line_height(px(16.))
        .text_color(muted)
        .child(text.into())
}

/// A line under a page's title on what the page is for.
fn page_lede(text: &'static str, muted: Hsla) -> Div {
    div()
        .mt_1p5()
        .text_size(SETTING_TEXT_SIZE)
        .line_height(px(18.))
        .text_color(muted)
        .child(text)
}

/// A setting's row in a group: its name with what it does below it, and its control on
/// the right.
fn setting_row(
    name: &'static str,
    description: Option<&'static str>,
    control: impl IntoElement,
    muted: Hsla,
) -> Div {
    h_flex()
        .min_h(SETTING_ROW_HEIGHT)
        .px(SETTING_ROW_INSET)
        .py_2p5()
        .gap_4()
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .gap_0p5()
                .child(setting_label(name))
                .children(description.map(|description| setting_note(description, muted))),
        )
        .child(div().flex_none().child(control))
}

/// A group of rows under an optional heading: an outline around them, with a line
/// between each two that starts where their text does.
fn settings_section(
    heading: Option<&'static str>,
    rows: Vec<AnyElement>,
    border: Hsla,
    muted: Hsla,
) -> Div {
    let rows = rows.into_iter().enumerate().map(|(ix, row)| {
        div().relative().child(row).when(ix > 0, |row| {
            row.child(
                div()
                    .absolute()
                    .top_0()
                    .left(SETTING_ROW_INSET)
                    .right_0()
                    .h(px(1.))
                    .bg(border),
            )
        })
    });
    v_flex()
        .mt_6()
        .children(heading.map(|heading| {
            div()
                .mb_2()
                .mx_0p5()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(heading)
        }))
        .child(
            v_flex()
                .rounded(px(10.))
                .border_1()
                .border_color(border)
                .overflow_hidden()
                .children(rows),
        )
}

/// `path` with the home folder written as `~`, as the user would type it.
fn shown_path(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
