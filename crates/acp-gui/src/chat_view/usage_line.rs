//! The plan usage line under the prompt box: how much of each of its plan's limits the
//! login of the session's agent has left, and when each starts over, as the agent pushes
//! them (`acp_core::Event::RateLimits`). The line is left out while the agent has said
//! nothing, and for a login without plan limits, such as an API key.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use acp_core::{RateLimitWindow, Setting, SettingKind};
use chrono::{DateTime, Local, TimeZone};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::*;

use crate::chat_view::ChatView;
use crate::session::Session;

/// Gap between the prompt box and the line.
pub(super) const USAGE_LINE_GAP: Pixels = px(4.);
const USAGE_LINE_HEIGHT: Pixels = px(20.);
/// From the box's left edge, so the first name is level with the model button's mark.
const USAGE_LINE_INSET: Pixels = px(16.);
const USAGE_LINE_END_PAD: Pixels = px(6.);
const USAGE_ITEM_GAP: Pixels = px(14.);
/// Between a window's name, what is left of it, and when it starts over.
const USAGE_PART_GAP: Pixels = px(6.);
/// Below the line, in place of the 12px below the box when there is no line.
pub(super) const USAGE_LINE_BOTTOM_PAD: Pixels = px(8.);

const FIVE_HOURS: u32 = 5 * 60;
const ONE_WEEK: u32 = 7 * 24 * 60;

/// What the line needs between pushes: the clock that draws it again when a window it
/// shows starts over, or the day changes and "resets Mon 09:00" becomes "resets 09:00".
#[derive(Default)]
pub(super) struct UsageLineState {
    /// The system shows times on a 24-hour clock. Read when the limits come and when
    /// the clock wakes, not on every frame, as macOS works it out from the locale.
    hour24: bool,
    clock: Option<Task<()>>,
}

impl ChatView {
    /// The agent of `agent` pushed its limits: they replace the ones before.
    pub(super) fn set_rate_limits(
        &mut self,
        agent: usize,
        windows: Vec<RateLimitWindow>,
        cx: &mut Context<Self>,
    ) {
        self.agents[agent].rate_limits = windows;
        self.usage.hour24 = hour24();
        self.usage.clock = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(wait)) = this.update(cx, |this, _| this.next_usage_change()) else {
                    break;
                };
                cx.background_executor().timer(wait).await;
                let woke = this.update(cx, |this, cx| {
                    this.usage.hour24 = hour24();
                    cx.notify();
                });
                if woke.is_err() {
                    break;
                }
            }
        }));
    }

    /// How long until the line of any agent reads differently on its own: a window
    /// starts over, or midnight comes. None while no agent has limits to show.
    fn next_usage_change(&self) -> Option<Duration> {
        let windows: Vec<_> = self
            .agents
            .iter()
            .flat_map(|slot| &slot.rate_limits)
            .collect();
        if windows.is_empty() {
            return None;
        }
        let now = SystemTime::now();
        windows
            .iter()
            .filter_map(|window| window.resets_at)
            .filter(|&at| at > now)
            .chain(next_midnight(Local::now()))
            .min()
            .and_then(|at| at.duration_since(now).ok())
            // A second past it, so the line is drawn once it has gone by.
            .map(|wait| wait + Duration::from_secs(1))
    }

    /// The line for `session`, from the limits its agent pushed last, or none while
    /// there is nothing to show.
    pub(super) fn render_usage_line(
        &self,
        session: &Session,
        settings: &[Setting],
        cx: &App,
    ) -> Option<Div> {
        let windows = &self.agents[session.agent].rate_limits;
        let model = settings
            .iter()
            .find(|setting| setting.kind == SettingKind::Model)
            .and_then(Setting::chosen)
            .map(|choice| [choice.id.as_str(), choice.name.as_str()]);
        let shown: Vec<_> = windows
            .iter()
            .filter(|window| applies_to(window, model.as_ref().map(|names| &names[..])))
            .collect();
        if shown.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let (foreground, muted) = (theme.foreground, theme.muted_foreground);
        let now = Local::now();
        let hour24 = self.usage.hour24;
        let tabular = FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
        Some(
            div()
                .mt(USAGE_LINE_GAP)
                .h(USAGE_LINE_HEIGHT)
                .pl(USAGE_LINE_INSET)
                .pr(USAGE_LINE_END_PAD)
                .flex()
                .items_center()
                .gap(USAGE_ITEM_GAP)
                .overflow_hidden()
                .text_xs()
                .text_color(muted)
                .children(shown.into_iter().map(|window| {
                    let resets_at = window.resets_at.map(DateTime::<Local>::from);
                    let tip = SharedString::from(tooltip(window, resets_at, now, hour24));
                    div()
                        .id(ElementId::Name(format!("usage-{}", window.id).into()))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(USAGE_PART_GAP)
                        .whitespace_nowrap()
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .child(short_label(window))
                        .child(
                            div()
                                .text_color(foreground)
                                .font_features(tabular.clone())
                                .child(format!("{}% left", left_percent(window, resets_at, now))),
                        )
                        .children(
                            resets_at
                                .filter(|&at| at > now)
                                .map(|at| format!("· resets {}", short_time(at, now, hour24))),
                        )
                })),
        )
    }
}

/// A window shows unless only some models count against it, and the session's model
/// isn't one of them: Claude's Fable window shows while the model is Fable. `model` is
/// the id and the name of the model in use, None while the agent hasn't said.
fn applies_to(window: &RateLimitWindow, model: Option<&[&str]>) -> bool {
    let Some(only) = &window.model else {
        return true;
    };
    let only = only.to_lowercase();
    model.is_some_and(|names| names.iter().any(|name| name.to_lowercase().contains(&only)))
}

/// "5h", "Weekly", or for a model's own weekly window the model's name, "Fable": the
/// agents' own labels ("Codex 5h limit", "Weekly · all models") are kept for the tooltip.
fn short_label(window: &RateLimitWindow) -> String {
    let span = match window.window_minutes {
        Some(FIVE_HOURS) => "5h".to_string(),
        Some(ONE_WEEK) => "Weekly".to_string(),
        Some(minutes) if minutes % (24 * 60) == 0 => format!("{}d", minutes / (24 * 60)),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
        None => return window.label.clone(),
    };
    match &window.model {
        Some(model) if window.window_minutes == Some(ONE_WEEK) => model.clone(),
        Some(model) => format!("{model} {span}"),
        None => span,
    }
}

/// What is left of the window, in whole percent. A window whose reset has gone by has
/// started over, though the agent only reads it again after its next turn.
fn left_percent<Tz: TimeZone>(
    window: &RateLimitWindow,
    resets_at: Option<DateTime<Tz>>,
    now: DateTime<Tz>,
) -> u32 {
    if resets_at.is_some_and(|at| at <= now) {
        return 100;
    }
    (100. - window.used_percent).clamp(0., 100.).round() as u32
}

/// "15:40" today, "Mon 09:00" within the week, "Oct 30" after that.
fn short_time<Tz: TimeZone>(at: DateTime<Tz>, now: DateTime<Tz>, hour24: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let days = (at.date_naive() - now.date_naive()).num_days();
    if days == 0 {
        clock_time(&at, hour24)
    } else if days < 7 {
        format!("{} {}", at.format("%a"), clock_time(&at, hour24))
    } else {
        at.format("%b %-d").to_string()
    }
}

/// The agent's own name for the window, how much is used, and the whole date it starts
/// over: "5-hour limit · 38% used · resets today at 15:40".
fn tooltip<Tz: TimeZone>(
    window: &RateLimitWindow,
    resets_at: Option<DateTime<Tz>>,
    now: DateTime<Tz>,
    hour24: bool,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let used = 100 - left_percent(window, resets_at.clone(), now.clone());
    let mut tip = format!("{} · {used}% used", window.label);
    if let Some(at) = resets_at {
        let day = if at.date_naive() == now.date_naive() {
            "today".to_string()
        } else {
            at.format("%a, %b %-d").to_string()
        };
        let verb = if at > now { "resets" } else { "started over" };
        tip.push_str(&format!(" · {verb} {day} at {}", clock_time(&at, hour24)));
    }
    tip
}

fn clock_time<Tz: TimeZone>(at: &DateTime<Tz>, hour24: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    at.format(if hour24 { "%H:%M" } else { "%-I:%M %p" })
        .to_string()
}

/// The coming midnight where the user is.
fn next_midnight(now: DateTime<Local>) -> Option<SystemTime> {
    let tomorrow = now.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
    // A midnight that a clock change skips comes as the first time after it.
    let at = tomorrow.and_local_timezone(Local).earliest()?;
    Some(at.into())
}

/// Whether macOS shows times on a 24-hour clock, by the user's region and the 24-hour
/// switch in System Settings: the hour of the locale's own pattern carries an `a` for AM
/// and PM on a 12-hour clock.
#[cfg(target_os = "macos")]
fn hour24() -> bool {
    use objc2_foundation::{NSDateFormatter, NSLocale, ns_string};
    let locale = NSLocale::autoupdatingCurrentLocale();
    NSDateFormatter::dateFormatFromTemplate_options_locale(ns_string!("j"), 0, Some(&locale))
        .is_none_or(|pattern| !pattern.to_string().contains('a'))
}

#[cfg(not(target_os = "macos"))]
fn hour24() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, TimeZone as _};

    use acp_core::RateLimitWindow;

    // Not a glob: gpui's own `test` attribute would shadow the standard one.
    use super::{applies_to, left_percent, short_label, short_time, tooltip};

    fn window(
        id: &str,
        label: &str,
        used: f64,
        minutes: u32,
        model: Option<&str>,
    ) -> RateLimitWindow {
        RateLimitWindow {
            id: id.into(),
            label: label.into(),
            used_percent: used,
            resets_at: None,
            window_minutes: Some(minutes),
            model: model.map(Into::into),
        }
    }

    fn at(day: u32, hour: u32, minute: u32) -> chrono::DateTime<FixedOffset> {
        // October 2026 in UTC+8; Oct 22 is a Thursday.
        FixedOffset::east_opt(8 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, day, hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn names_windows_as_the_design_does() {
        assert_eq!(
            short_label(&window("five_hour", "5-hour limit", 38., 300, None)),
            "5h"
        );
        assert_eq!(
            short_label(&window(
                "codex:secondary",
                "Codex Weekly limit",
                59.,
                10080,
                None
            )),
            "Weekly"
        );
        assert_eq!(
            short_label(&window(
                "seven_day:Fable",
                "Weekly · Fable",
                27.,
                10080,
                Some("Fable")
            )),
            "Fable"
        );
        assert_eq!(
            short_label(&window(
                "codex_spark:primary",
                "Spark 5h limit",
                3.,
                300,
                Some("Spark")
            )),
            "Spark 5h"
        );
        let unknown = RateLimitWindow {
            window_minutes: None,
            ..window("x", "Daily limit", 1., 0, None)
        };
        assert_eq!(short_label(&unknown), "Daily limit");
    }

    #[test]
    fn shows_a_model_window_only_while_that_model_is_in_use() {
        let fable = window(
            "seven_day:Fable",
            "Weekly · Fable",
            27.,
            10080,
            Some("Fable"),
        );
        let weekly = window("seven_day", "Weekly · all models", 59., 10080, None);
        assert!(applies_to(&fable, Some(&["fable", "Fable 5.1"])));
        assert!(applies_to(&fable, Some(&["claude-fable-5-1", "Fable"])));
        assert!(!applies_to(&fable, Some(&["opus", "Opus 5.5"])));
        assert!(!applies_to(&fable, None));
        assert!(applies_to(&weekly, None));
    }

    #[test]
    fn counts_what_is_left_and_a_reset_gone_by_as_whole() {
        let five = window("five_hour", "5-hour limit", 38.4, 300, None);
        let now = at(22, 14, 0);
        assert_eq!(left_percent(&five, Some(at(22, 15, 40)), now), 62);
        assert_eq!(left_percent(&five, Some(at(22, 13, 0)), now), 100);
        let spent = window("five_hour", "5-hour limit", 100., 300, None);
        assert_eq!(left_percent(&spent, None, now), 0);
    }

    #[test]
    fn writes_the_reset_by_how_far_off_it_is() {
        let now = at(22, 14, 0);
        assert_eq!(short_time(at(22, 15, 40), now, true), "15:40");
        assert_eq!(short_time(at(22, 15, 40), now, false), "3:40 PM");
        assert_eq!(short_time(at(26, 9, 0), now, true), "Mon 09:00");
        assert_eq!(short_time(at(30, 9, 0), now, true), "Oct 30");
    }

    #[test]
    fn tooltip_has_the_whole_date_and_the_share_used() {
        let now = at(22, 14, 0);
        let five = window("five_hour", "5-hour limit", 38., 300, None);
        assert_eq!(
            tooltip(&five, Some(at(22, 15, 40)), now, true),
            "5-hour limit · 38% used · resets today at 15:40"
        );
        let weekly = window("seven_day", "Weekly · all models", 59., 10080, None);
        assert_eq!(
            tooltip(&weekly, Some(at(26, 9, 0)), now, true),
            "Weekly · all models · 59% used · resets Mon, Oct 26 at 09:00"
        );
        assert_eq!(
            tooltip(&five, Some(at(22, 13, 0)), now, true),
            "5-hour limit · 0% used · started over today at 13:00"
        );
    }
}
