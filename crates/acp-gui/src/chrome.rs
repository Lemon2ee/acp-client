//! The window around the sidebar and the chat: the title bar row the app draws itself on
//! macOS, the traffic lights in it, the blurred background, and the theme's light or dark
//! mode with the sidebar's and the chat's two grays.

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::*;

use crate::config::{AppSettings, Appearance};

/// On macOS the window has no title bar row: the sidebar and the chat both start at the
/// top edge, with the traffic lights in the window's top row. Elsewhere the system
/// title bar stays.
pub(crate) const OWN_TITLE_BAR: bool = cfg!(target_os = "macos");

/// Whether the window can blur what is behind it. On macOS gpui puts an
/// `NSVisualEffectView` under its Metal layer and the window server blurs; elsewhere
/// the window stays opaque, as gpui's blur there is a guess: Windows uses an undocumented
/// acrylic attribute, Wayland blurs only under KDE, and X11 just turns see-through.
pub(crate) const WINDOW_BLUR: bool = cfg!(target_os = "macos");
/// How much of the blur the sidebar's and the chat's colors cover while the window is
/// blurred. The sidebar lets more of it through, as macOS sidebars do, and the chat
/// little enough that its text reads as well as on an opaque window.
pub(crate) const BLURRED_SIDEBAR_ALPHA: f32 = 0.6;
pub(crate) const BLURRED_CHAT_ALPHA: f32 = 0.85;

pub(crate) fn window_background(blurred: bool) -> WindowBackgroundAppearance {
    if blurred {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// Where the traffic lights go, and the top row that holds them, with as much space to
/// the lights' left and below them as above.
#[derive(Clone, Copy)]
pub(crate) struct TrafficLights {
    /// The close button's top-left corner.
    pub(crate) origin: Point<Pixels>,
    /// Each light's width and height.
    size: Pixels,
    pub(crate) row_height: Pixels,
}

impl TrafficLights {
    pub(crate) fn new(size: f32) -> Self {
        // The close button's center sits 21.5pt from the window's top and left edges.
        let inset = 21.5 - size / 2.;
        Self {
            origin: point(px(inset), px(inset)),
            size: px(size),
            row_height: px(2. * inset + size),
        }
    }

    /// Where the zoom button ends, from the window's left edge. gpui puts the close
    /// button at `origin` and keeps AppKit's spacing for the other two, which leaves
    /// 9pt between lights on macOS 27 (the fallback).
    pub(crate) fn end(&self, window: &Window) -> Pixels {
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::{NSView, NSWindowButton};
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};

            // `Window::window_handle` is gpui's own handle, not the platform's.
            if let Ok(handle) = HasWindowHandle::window_handle(window)
                && let RawWindowHandle::AppKit(handle) = handle.as_raw()
                // SAFETY: the handle's view is the window's content view, which lives as
                // long as the window.
                && let Some(ns_window) = unsafe { handle.ns_view.cast::<NSView>().as_ref() }.window()
                && let Some(close) = ns_window.standardWindowButton(NSWindowButton::CloseButton)
                && let Some(zoom) = ns_window.standardWindowButton(NSWindowButton::ZoomButton)
            {
                let (close, zoom) = (close.frame(), zoom.frame());
                return self.origin.x
                    + px((zoom.origin.x + zoom.size.width - close.origin.x) as f32);
            }
        }
        let _ = window;
        self.origin.x + self.size * 3. + px(2. * 9.)
    }
}

/// The traffic lights' size as AppKit lays them out: 14pt on macOS 27, which is also
/// the fallback.
pub(crate) fn traffic_light_size() -> f32 {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSWindow, NSWindowButton, NSWindowStyleMask};
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        if let Some(mtm) = objc2::MainThreadMarker::new()
            && let Some(button) =
                NSWindow::standardWindowButton_forStyleMask(NSWindowButton::CloseButton, style, mtm)
        {
            return button.frame().size.height as f32;
        }
    }
    14.
}

/// Makes `strip` stand in for the title bar the window doesn't have: dragging it moves
/// the window, and a double-click does what the system setting for title bars says.
/// The strip must not hold anything clickable.
pub(crate) fn as_title_bar(strip: Div) -> Div {
    strip.on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count == 2 {
            window.titlebar_double_click();
        } else {
            // AppKit moves the window with the mouse from this press until the button
            // comes up, and keeps that mouse-up to itself. A press that doesn't move
            // leaves the window where it is.
            window.start_window_move();
        }
    })
}

/// Puts the theme in the mode the settings ask for. The theme is the app's, so this
/// repaints every window.
pub(crate) fn apply_appearance(window: &mut Window, cx: &mut App) {
    match cx.global::<AppSettings>().appearance {
        Appearance::System => Theme::sync_system_appearance(Some(window), cx),
        Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
    }
}

/// `config` with the sidebar and the chat in two grays. In both modes the sidebar is the
/// darker one, by about the same small step in lightness.
pub(crate) fn with_panel_colors(
    config: &ThemeConfig,
    sidebar: &'static str,
    chat: &'static str,
) -> Rc<ThemeConfig> {
    let mut config = config.clone();
    config.colors.sidebar = Some(sidebar.into());
    config.colors.background = Some(chat.into());
    Rc::new(config)
}
