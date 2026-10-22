//! Minimal gpui front end: a sidebar of sessions beside a chat, driving ACP agents.
//! Usage: `acp-gui [claude | codex | "<agent command>"]`. With no argument it runs
//! both Claude and Codex, and the model menu picks between them.

mod chat_view;
mod chrome;
mod config;
mod debug;
mod hotkey;
#[cfg(target_os = "macos")]
mod launch;
mod session;

use std::borrow::Cow;

use acp_core::{AcpAgent, Harness};
use gpui_kit::base::actions::{Cancel, Confirm, SelectDown, SelectUp};
use gpui_kit::component::Theme;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::chrome::{
    OWN_TITLE_BAR, TrafficLights, WINDOW_BLUR, apply_appearance, traffic_light_size,
    window_background, with_panel_colors,
};
use crate::config::{AppSettings, load_settings};

// The icons `mode_icon`, the settings and the tool calls' kinds use beyond those in the
// component library's own bundle.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Hand,
        FilePen,
        ListChecks,
        FolderPen,
        ShieldCheck,
        ShieldOff,
        Shield,
        SlidersHorizontal,
        Keyboard,
        Monitor,
        Pencil,
        Trash,
        ArrowRightLeft,
        Terminal,
        Lightbulb,
        ToggleRight,
        Wrench,
        Layers,
        CheckCheck
    ]
);

/// The component library's icons, and the extra ones.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(icon) => Ok(Some(icon)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

actions!(acp_gui, [Quit, OpenSettings, CloseSettings]);

#[cfg(feature = "profile")]
actions!(acp_gui, [CycleFrameOverlay]);

/// With `profile`, sends the agent's answer times and the protocol's warnings to
/// stderr, and every info span and event to Tracy while it is connected.
#[cfg(feature = "profile")]
fn start_tracing() {
    use tracing_subscriber::filter::{LevelFilter, Targets};
    use tracing_subscriber::prelude::*;

    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(
            Targets::new()
                .with_target("acp_core", LevelFilter::INFO)
                .with_target("agent_client_protocol", LevelFilter::WARN),
        );
    tracing_subscriber::registry()
        .with(stderr)
        // Also starts Tracy's client, which gpui's zones need before the first frame.
        .with(tracing_tracy::TracyLayer::default().with_filter(LevelFilter::INFO))
        .init();
}

/// With `profile`, Cmd+Option+F cycles gpui's frame-time overlay: hidden, the last
/// frame's draw time, then that with the slowest 1% and 10%, the worst, and a count.
#[cfg(feature = "profile")]
fn bind_frame_overlay(cx: &mut App) {
    cx.on_action(|_: &CycleFrameOverlay, cx| {
        let Some(window) = cx.active_window() else {
            return;
        };
        // A key press is dispatched from inside the window's own update, where the
        // window can't be updated again.
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| window.cycle_debug_frame_overlay_mode());
        });
    });
    cx.bind_keys([KeyBinding::new("cmd-alt-f", CycleFrameOverlay, None)]);
}

fn main() {
    // First, while the process has only this thread: it may set PATH.
    #[cfg(target_os = "macos")]
    let cwd = launch::session_folder();
    #[cfg(not(target_os = "macos"))]
    let cwd = std::env::current_dir().expect("no current directory");
    #[cfg(feature = "profile")]
    start_tracing();
    // One agent by name, or both harnesses to pick between from the model menu.
    let name = std::env::args().nth(1);
    let agents: Vec<(String, AcpAgent)> = match &name {
        Some(name) => match acp_core::agent_for(name) {
            Ok(agent) => vec![(name.clone(), agent)],
            Err(err) => {
                eprintln!("bad agent command: {err}");
                std::process::exit(2);
            }
        },
        None => Harness::ALL
            .into_iter()
            .map(|harness| (harness.name().to_string(), harness.agent()))
            .collect(),
    };
    let title = match &name {
        Some(name) => format!("ACP client: {name}"),
        None => "ACP client".to_string(),
    };

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            // Nothing quits on Cmd+Q unless the app says so: gpui-kit leaves the shortcut
            // to the app, and with no menus set AppKit has no Quit item to match it to.
            cx.on_action(|_: &Quit, cx| cx.quit());
            // Cmd+, opens the settings anywhere (see `ChatView::new`), and leaves them
            // while they are open, as does Esc.
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-,", CloseSettings, Some("Settings")),
                KeyBinding::new("escape", CloseSettings, Some("Settings")),
            ]);
            // In the effort and mode menus, Up and Down move the light and Enter or Space
            // picks the lit row. Esc is the popover's, which closes it. These are the
            // component library's own actions, as its menus take them.
            cx.bind_keys([
                KeyBinding::new("up", SelectUp, Some("PickerMenu")),
                KeyBinding::new("down", SelectDown, Some("PickerMenu")),
                KeyBinding::new("enter", Confirm { secondary: false }, Some("PickerMenu")),
                KeyBinding::new("space", Confirm { secondary: false }, Some("PickerMenu")),
            ]);
            // On a permission card's option, reached with Tab, Up and Down move between
            // its options, Enter or Space answers with that one, and Esc says no. From
            // the empty prompt the card takes the same keys but Space, which is typing
            // (see `ChatView::card_keystroke`).
            cx.bind_keys([
                KeyBinding::new("up", SelectUp, Some("PermissionCard")),
                KeyBinding::new("down", SelectDown, Some("PermissionCard")),
                KeyBinding::new(
                    "enter",
                    Confirm { secondary: false },
                    Some("PermissionCard"),
                ),
                KeyBinding::new(
                    "space",
                    Confirm { secondary: false },
                    Some("PermissionCard"),
                ),
                KeyBinding::new("escape", Cancel, Some("PermissionCard")),
            ]);
            #[cfg(feature = "profile")]
            bind_frame_overlay(cx);
            cx.set_menus([Menu::new("ACP client").items([
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                #[cfg(feature = "profile")]
                MenuItem::action("Cycle frame-time overlay", CycleFrameOverlay),
                MenuItem::action("Quit ACP client", Quit),
            ])]);
            cx.set_global(load_settings());
            Theme::update(cx, |theme| {
                theme.light_theme = with_panel_colors(&theme.light_theme, "#f7f7f7", "#fcfcfc");
                theme.dark_theme = with_panel_colors(&theme.dark_theme, "#111111", "#151515");
            });
            let lights = TrafficLights::new(traffic_light_size());
            // Blurred from the start, where the platform can blur and the settings say so.
            // The window opens that way, so its first frame already is.
            let blur = WINDOW_BLUR && cx.global::<AppSettings>().window_blur;
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1000.), px(820.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    appears_transparent: OWN_TITLE_BAR,
                    traffic_light_position: OWN_TITLE_BAR.then_some(lights.origin),
                }),
                // The app moves the window itself (see `as_title_bar`). Otherwise AppKit
                // does it from the strip around the traffic lights, and on macOS 27 it
                // holds back every click there while it waits for a double-click.
                app_owns_titlebar_drag: OWN_TITLE_BAR,
                window_background: window_background(blur),
                ..Default::default()
            };
            // What `gpui_kit::open_window` does, but the root paints no background of its
            // own: the chat view paints every surface, and the root's opaque one under
            // them would hide the blur.
            cx.open_window(options, |window, cx| {
                // Light or dark as the settings say, and for System again whenever the
                // system's setting changes.
                apply_appearance(window, cx);
                window.observe_window_appearance(apply_appearance).detach();
                let lights_end = lights.end(window);
                let view = cx.new(|cx| {
                    ChatView::new(agents, cwd, blur, lights.row_height, lights_end, window, cx)
                });
                cx.new(|cx| gpui_kit::base::Root::new(view, window, cx).bg(transparent_black()))
            })
            .expect("failed to open the window");
            cx.activate(true);
        });
}
