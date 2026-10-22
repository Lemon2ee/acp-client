//! The settings, which take the whole window in place of the sidebar and the chat while
//! open: the General, Agents and Keyboard pages, the rows and groups they are made of, and
//! the recorder for the system-wide shortcut.

use std::path::Path;

use gpui_kit::component::button::Button;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::CloseSettings;
use crate::chat_view::ChatView;
use crate::chat_view::pickers::agent_logo;
use crate::chat_view::sidebar::ResizeSidebar;
use crate::chrome::{WINDOW_BLUR, as_title_bar};
use crate::config::{AppSettings, Appearance, SETTINGS, config_folder};
use crate::hotkey::{GLOBAL_SHORTCUT, Refusal, parse_shortcut, shadows_menus};

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
/// The box the system-wide shortcut is recorded in, and the button in it that clears it.
const RECORDER_WIDTH: Pixels = px(168.);
const RECORDER_HEIGHT: Pixels = px(28.);
const CLEAR_BUTTON_SIZE: Pixels = px(18.);
/// The colors of what is said under a shortcut that was turned down or only half works.
/// The theme's danger and warning are too light on the light theme's white for 12px text.
const REFUSED_DARK: u32 = 0xf87171;
const REFUSED_LIGHT: u32 = 0xdc2626;
const WARNED_DARK: u32 = 0xfbbf24;
const WARNED_LIGHT: u32 = 0xb45309;
/// System Settings' Keyboard pane, where its Keyboard Shortcuts are.
const KEYBOARD_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Keyboard-Settings.extension";

/// What the settings keep for the window while it runs.
pub(crate) struct SettingsPageState {
    /// The settings take the whole window while open, in place of the sidebar and the
    /// chat, which come back as they were.
    pub(crate) open: bool,
    /// The settings' page on show, kept while they are closed.
    page: SettingsPage,
    /// Holds keyboard focus while the settings are open, so Esc and Cmd+, reach them:
    /// the prompt box, which has it otherwise, isn't drawn then.
    focus: FocusHandle,
    recorder: ShortcutRecorder,
}

impl SettingsPageState {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            open: false,
            page: SettingsPage::General,
            focus: cx.focus_handle(),
            recorder: ShortcutRecorder::default(),
        }
    }
}

/// The keyboard page's recorder for the system-wide shortcut, while it takes keys.
#[derive(Default)]
struct ShortcutRecorder {
    /// Keys go to the recorder rather than to the app's shortcuts, and the system-wide
    /// shortcut is let go of, so that pressing it records it.
    recording: bool,
    /// The modifiers held down, shown until a key comes with them.
    held: Modifiers,
    /// The last keystroke turned down, and why. The shortcut set before stays.
    refused: Option<(Keystroke, Refusal)>,
}

/// A page of the settings.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
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
        self.settings_page.open = true;
        window.focus(&self.settings_page.focus, cx);
        cx.notify();
    }

    /// Brings back the sidebar and the chat as they were, the prompt box focused.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        self.settings_page.open = false;
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
        let page = self.settings_page.page;

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
                    this.stop_recording(cx);
                    this.settings_page.page = item;
                    cx.notify();
                }))
        });
        let pages = div()
            .w(self.sidebar.width)
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
            SettingsPage::Keyboard => self.keyboard_page(cx),
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
            .track_focus(&self.settings_page.focus)
            .key_context("Settings")
            .on_action(
                cx.listener(|this, _: &CloseSettings, window, cx| this.close_settings(window, cx)),
            )
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                let recorder = &mut this.settings_page.recorder;
                if recorder.recording {
                    recorder.held = event.modifiers;
                    cx.notify();
                }
            }))
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

    /// The system-wide shortcut, where there can be one, and the app's own shortcuts,
    /// which can't be changed yet.
    fn keyboard_page(&self, cx: &mut Context<Self>) -> Div {
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
        let (lede, heading) = if GLOBAL_SHORTCUT {
            (
                "Bring ACP client forward from any app. The shortcuts inside it can't be changed yet.",
                Some("In ACP client"),
            )
        } else {
            ("Shortcuts can't be changed yet.", None)
        };
        let mut page = v_flex().child(page_lede(lede, muted));
        if GLOBAL_SHORTCUT {
            page = page.child(self.global_shortcut_section(cx));
        }
        page = page.child(settings_section(
            heading,
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

    /// The switch for the system-wide shortcut, the box it is recorded in, what is wrong
    /// with it if anything is, and the switch for hiding the app with it.
    fn global_shortcut_section(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, border, hover) = (theme.muted_foreground, theme.border, theme.secondary);
        let (field, field_border, ring) = (theme.background, theme.input, theme.ring);
        let (refused_color, warned_color): (Hsla, Hsla) = if theme.is_dark() {
            (rgb(REFUSED_DARK).into(), rgb(WARNED_DARK).into())
        } else {
            (rgb(REFUSED_LIGHT).into(), rgb(WARNED_LIGHT).into())
        };
        let settings = cx.global::<AppSettings>();
        let (on, hide) = (settings.bring_to_front, settings.hide_when_in_front);
        let saved = settings
            .bring_to_front_shortcut
            .as_deref()
            .and_then(parse_shortcut);
        let recorder = &self.settings_page.recorder;
        let recording = recorder.recording;
        let refused = recorder.refused.as_ref().filter(|_| recording);
        // Said only of the shortcut the settings still name.
        let failed = self
            .global_shortcut
            .failed
            .as_ref()
            .filter(|(keystroke, _)| on && !recording && saved.as_ref() == Some(keystroke));

        let note = if !on {
            "Turn on to record one."
        } else if recording {
            "Press the keys together. Esc cancels, ⌫ clears."
        } else if failed.is_some() {
            "Not active right now."
        } else if saved.is_some() {
            "Works from any app while ACP client runs."
        } else {
            "None yet. Click the field to record one."
        };

        let text = |text: String, color: Hsla| {
            div()
                .text_size(SETTING_NOTE_SIZE)
                .line_height(px(16.))
                .whitespace_nowrap()
                .text_color(color)
                .child(text)
        };
        // The modifiers of the next try, once any are down, over the keys turned down.
        let shown = if recording && recorder.held.modified() {
            let held = Keystroke {
                modifiers: recorder.held,
                ..Default::default()
            };
            text(format!("{}…", Kbd::format(&held)), theme.foreground)
                .font_weight(FontWeight::MEDIUM)
                .into_any_element()
        } else if let Some((keystroke, _)) = refused {
            text(Kbd::format(keystroke), refused_color)
                .font_weight(FontWeight::MEDIUM)
                .into_any_element()
        } else if recording {
            text("Type shortcut…".into(), muted).into_any_element()
        } else if let Some(saved) = &saved {
            Kbd::new(saved.clone()).into_any_element()
        } else if on {
            text("Record shortcut".into(), muted).into_any_element()
        } else {
            text("Not set".into(), muted).into_any_element()
        };
        let clearable = on && !recording && saved.is_some();
        let label = if recording {
            "Recording a shortcut".to_string()
        } else if let Some(saved) = &saved {
            format!("Shortcut {}. Click to change it.", Kbd::format(saved))
        } else {
            "Record a shortcut".to_string()
        };
        let recorder_box = div()
            .id("shortcut-recorder")
            .role(Role::Button)
            .aria_label(label)
            .w(RECORDER_WIDTH)
            .h(RECORDER_HEIGHT)
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .pl_2()
            // Clear of the button that clears it.
            .pr(if clearable {
                CLEAR_BUTTON_SIZE + px(8.)
            } else {
                px(8.)
            })
            .rounded(px(6.))
            .border_1()
            .bg(field)
            .map(|recorder_box| match (recording, refused.is_some()) {
                (true, refused) => {
                    let color = if refused { refused_color } else { ring };
                    recorder_box.border_color(color).shadow(vec![
                        BoxShadow::new(px(0.), px(0.), color.opacity(0.3)).spread_radius(px(3.)),
                    ])
                }
                (false, _) => recorder_box.border_color(field_border),
            })
            .child(shown)
            .when(on, |recorder_box| {
                recorder_box.cursor_pointer().on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_recording(window, cx)
                    }),
                )
            })
            .when(clearable, |recorder_box| {
                recorder_box.child(
                    div()
                        .id("clear-shortcut")
                        .role(Role::Button)
                        .aria_label("Clear shortcut")
                        .absolute()
                        .top(px(4.))
                        .right(px(4.))
                        .size(CLEAR_BUTTON_SIZE)
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .text_color(muted)
                        .cursor_pointer()
                        .hover(|style| style.bg(hover))
                        .child(Icon::new(IconName::Close).xsmall())
                        .on_click(|_, _, cx| {
                            // Not a click on the box, which would start recording.
                            cx.stop_propagation();
                            AppSettings::change(cx, |settings| {
                                settings.bring_to_front_shortcut = None
                            });
                        }),
                )
            });

        // What is wrong, if anything, said under the shortcut's row.
        let problem = if let Some((keystroke, refusal)) = refused {
            let kept = match &saved {
                Some(saved) => format!("Still using {}.", Kbd::format(saved)),
                None => "No shortcut is set yet.".to_string(),
            };
            let action = matches!(refusal, Refusal::System(_)).then(|| {
                Button::new("keyboard-settings")
                    .label("Open Keyboard Settings")
                    .icon(Icon::new(IconName::ExternalLink))
                    .xsmall()
                    .outline()
                    .on_click(|_, _, cx| cx.open_url(KEYBOARD_SETTINGS_URL))
                    .into_any_element()
            });
            Some((
                IconName::CircleAlert,
                refused_color,
                refusal_message(*refusal, keystroke),
                Some(kept),
                action,
            ))
        } else if let Some((keystroke, refusal)) = failed {
            let retry = Button::new("retry-shortcut")
                .label("Try again")
                .icon(Icon::new(IconName::RotateCw))
                .xsmall()
                .outline()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.global_shortcut.sync(true, cx);
                    cx.notify();
                }))
                .into_any_element();
            Some((
                IconName::TriangleAlert,
                warned_color,
                failed_message(*refusal, keystroke),
                None,
                Some(retry),
            ))
        } else {
            saved
                .as_ref()
                .filter(|saved| on && !recording && shadows_menus(saved))
                .map(|saved| {
                    let message = format!(
                        "Most apps use ⌘ with one key for their own commands. While this is on, {} brings ACP client forward instead.",
                        Kbd::format(saved)
                    );
                    (IconName::TriangleAlert, warned_color, message, None, None)
                })
        };
        let problem = problem.map(|(icon, color, message, kept, action)| {
            h_flex()
                .items_start()
                .gap_2()
                .px(SETTING_ROW_INSET)
                .pb_3()
                .text_size(SETTING_NOTE_SIZE)
                .line_height(px(16.))
                .child(
                    Icon::new(icon)
                        .small()
                        .flex_none()
                        .mt(px(1.))
                        .text_color(color),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .gap_0p5()
                        .child(div().text_color(color).child(message))
                        .children(kept.map(|kept| div().text_color(muted).child(kept)))
                        .children(action.map(|action| div().mt_1p5().flex().child(action))),
                )
        });

        let rows = vec![
            // The whole row flips the switch, as the blur's does.
            setting_row(
                "Bring ACP client to the front",
                Some("From any app, with a shortcut of your own."),
                Switch::new("bring-to-front-value")
                    .checked(on)
                    .tab_stop(false),
                muted,
            )
            .id("bring-to-front")
            .role(Role::Switch)
            .aria_label("Bring ACP client to the front")
            .aria_toggled(on.into())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                AppSettings::change(cx, |settings| settings.bring_to_front = !on);
                if on {
                    this.stop_recording(cx);
                } else if cx.global::<AppSettings>().bring_to_front_shortcut.is_none() {
                    // There is nothing to turn on yet, so it starts with recording one.
                    this.start_recording(window, cx);
                }
            }))
            .into_any_element(),
            v_flex()
                .child(setting_row("Shortcut", Some(note), recorder_box, muted))
                .children(problem)
                .when(!on, |row| row.opacity(0.5))
                // A click outside the row stops recording, as the focus stays where it
                // is. One on Open Keyboard Settings under it doesn't, or the button would
                // be gone before the click is done.
                .when(recording, |row| {
                    row.on_mouse_down_out(
                        cx.listener(|this, _: &MouseDownEvent, _, cx| this.stop_recording(cx)),
                    )
                })
                .into_any_element(),
            setting_row(
                "Press again to hide",
                Some("When ACP client is already in front, the shortcut hides it."),
                Switch::new("hide-when-in-front-value")
                    .checked(hide)
                    .disabled(!on)
                    .tab_stop(false),
                muted,
            )
            .id("hide-when-in-front")
            .role(Role::Switch)
            .aria_label("Press again to hide")
            .aria_toggled(hide.into())
            .map(|row| {
                if on {
                    row.cursor_pointer().on_click(move |_, _, cx| {
                        AppSettings::change(cx, |settings| settings.hide_when_in_front = !hide)
                    })
                } else {
                    row.opacity(0.5)
                }
            })
            .into_any_element(),
        ];
        settings_section(Some("System-wide"), rows, border, muted)
    }

    /// Has the keys pressed go to the recorder, while the shortcut is on. The one set
    /// before is let go of meanwhile, so it can be recorded again.
    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.global::<AppSettings>().bring_to_front || self.settings_page.recorder.recording {
            return;
        }
        self.settings_page.recorder = ShortcutRecorder {
            recording: true,
            held: window.modifiers(),
            refused: None,
        };
        self.global_shortcut.pause();
        // Where the modifiers held come to.
        window.focus(&self.settings_page.focus, cx);
        cx.notify();
    }

    /// Stops recording, and holds the shortcut again: the one recorded, or the one before.
    pub(crate) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if !self.settings_page.recorder.recording {
            return;
        }
        self.settings_page.recorder = ShortcutRecorder::default();
        self.global_shortcut.resume(cx);
        cx.notify();
    }

    /// Takes a key pressed anywhere in the window, while recording, as the system-wide
    /// shortcut if it can be one. Returns whether the recorder took it, so that nothing
    /// else does: Cmd+Q is recorded, not obeyed.
    pub(crate) fn record_keystroke(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.settings_page.recorder.recording {
            return false;
        }
        let modifiers = keystroke.modifiers;
        match keystroke.key.as_str() {
            // A modifier pressed and let go on its own.
            "shift" | "control" | "alt" | "platform" | "function" => return true,
            "escape" if !modifiers.modified() => {
                self.stop_recording(cx);
                return true;
            }
            "backspace" | "delete" if !modifiers.modified() => {
                AppSettings::change(cx, |settings| settings.bring_to_front_shortcut = None);
                self.stop_recording(cx);
                return true;
            }
            _ => {}
        }
        // macOS sets fn on its own for the arrows and F-keys, and Carbon takes no fn.
        let keystroke = Keystroke {
            modifiers: Modifiers {
                function: false,
                ..modifiers
            },
            key: keystroke.key.clone(),
            key_char: None,
        };
        match self.global_shortcut.take(keystroke.clone(), cx) {
            Ok(()) => {
                self.settings_page.recorder = ShortcutRecorder::default();
                let text = keystroke.unparse();
                AppSettings::change(cx, move |settings| {
                    settings.bring_to_front_shortcut = Some(text.clone())
                });
            }
            Err(refusal) => {
                let recorder = &mut self.settings_page.recorder;
                recorder.refused = Some((keystroke, refusal));
                // Still down, but the keys turned down show until the modifiers change.
                recorder.held = Modifiers::default();
            }
        }
        cx.notify();
        true
    }
}

/// What the keyboard page says of a keystroke the recorder turned down.
fn refusal_message(refusal: Refusal, keystroke: &Keystroke) -> String {
    let keys = Kbd::format(keystroke);
    match refusal {
        Refusal::NoModifier => {
            format!("Add ⌘, ⌃ or ⌥. Taken system-wide, {keys} couldn't be typed in any other app.")
        }
        Refusal::InApp => {
            let listed = SHORTCUTS
                .iter()
                .find(|(_, listed)| parse_shortcut(listed).as_ref() == Some(keystroke));
            match listed {
                Some((name, _)) => format!("ACP client already uses {keys} for “{name}”."),
                None => format!("ACP client already uses {keys} itself."),
            }
        }
        Refusal::System(Some(name)) => format!(
            "{keys} is a macOS shortcut ({name}). Pick another, or turn it off in System Settings."
        ),
        Refusal::System(None) => {
            format!("{keys} is a macOS shortcut. Pick another, or turn it off in System Settings.")
        }
        Refusal::Taken => {
            format!("Another app already uses {keys}, so macOS won't give it to ACP client.")
        }
        Refusal::Refused(status) => {
            format!("macOS turned down {keys} (error {status}). Try one with ⌘ or ⌃.")
        }
        Refusal::NoKey => format!("No key on the current keyboard layout types {keys}."),
    }
}

/// What the keyboard page says of the shortcut set before, when macOS wouldn't give it
/// to the app this time.
fn failed_message(refusal: Refusal, keystroke: &Keystroke) -> String {
    match refusal {
        Refusal::Taken => format!(
            "Another app took {} first, so it does nothing for now.",
            Kbd::format(keystroke)
        ),
        refusal => refusal_message(refusal, keystroke),
    }
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
