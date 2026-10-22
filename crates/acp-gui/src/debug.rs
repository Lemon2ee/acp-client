//! Switches for finding what makes a live reply look wrong, read once from `ACP_GUI_DEBUG`
//! when the app starts. It takes words separated by commas or spaces:
//!
//! - `no-pace`: the agent's text shows the moment it arrives, as the agent sends it,
//!   instead of held back and shown at an even pace. It still fades in.
//! - `no-fade`: the agent's text shows without fading in.
//! - `no-enter`: new rows show at once instead of rising as they fade in.
//! - `no-thinking`: no "Thinking…" line under the last row.
//! - `no-follow`: the transcript stays where it is as text comes, instead of keeping the
//!   newest text in view.
//! - `log`: a line on stderr each time the transcript draws: the time since the last one,
//!   the last row's height, where the view is, and how much of the row's text shows.
//!
//! For example `ACP_GUI_DEBUG=no-pace,log cargo run --release -p acp-gui 2> frames.log`.

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The words `ACP_GUI_DEBUG` takes, for the message about one it doesn't.
const KNOWN: &str = "no-pace, no-fade, no-enter, no-thinking, no-follow, log";

/// Which of a live turn's effects are on, all of them unless `ACP_GUI_DEBUG` turns some
/// off, and whether frames are logged.
pub(crate) struct Switches {
    pub(crate) pace: bool,
    pub(crate) fade: bool,
    pub(crate) enter: bool,
    pub(crate) thinking: bool,
    pub(crate) follow: bool,
    pub(crate) log: bool,
}

pub(crate) fn switches() -> &'static Switches {
    static SWITCHES: OnceLock<Switches> = OnceLock::new();
    SWITCHES.get_or_init(|| {
        let value = std::env::var("ACP_GUI_DEBUG").unwrap_or_default();
        let switches = Switches::parse(&value);
        if !value.trim().is_empty() {
            eprintln!("ACP_GUI_DEBUG: {}", switches.describe());
        }
        switches
    })
}

impl Switches {
    fn parse(value: &str) -> Self {
        let mut switches = Self {
            pace: true,
            fade: true,
            enter: true,
            thinking: true,
            follow: true,
            log: false,
        };
        for word in value.split([',', ' ']).filter(|word| !word.is_empty()) {
            match word {
                "no-pace" => switches.pace = false,
                "no-fade" => switches.fade = false,
                "no-enter" => switches.enter = false,
                "no-thinking" => switches.thinking = false,
                "no-follow" => switches.follow = false,
                "log" => switches.log = true,
                _ => eprintln!("ACP_GUI_DEBUG: no switch `{word}`; there are {KNOWN}"),
            }
        }
        switches
    }

    /// What is on and off, printed once at start so a run's log says what it ran with.
    fn describe(&self) -> String {
        let on_off = |on: bool| if on { "on" } else { "off" };
        format!(
            "pace {}, fade {}, enter {}, thinking {}, follow {}, log {}",
            on_off(self.pace),
            on_off(self.fade),
            on_off(self.enter),
            on_off(self.thinking),
            on_off(self.follow),
            on_off(self.log),
        )
    }
}

/// The time since the transcript last drew, for `log`; `None` the first time.
pub(crate) fn since_last_frame() -> Option<Duration> {
    thread_local! {
        static LAST: Cell<Option<Instant>> = const { Cell::new(None) };
    }
    let now = Instant::now();
    LAST.with(|last| last.replace(Some(now)))
        .map(|last| now - last)
}
