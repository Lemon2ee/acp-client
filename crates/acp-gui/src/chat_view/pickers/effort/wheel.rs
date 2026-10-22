//! How the effort meter reads a scroll over it: a level for each notch of a mouse wheel,
//! one for each 40px a trackpad's fingers move, nothing for a fling's momentum, and
//! nothing for scrolling that started somewhere else and only passes over the meter.

use std::time::{Duration, Instant};

use gpui_kit::{ScrollDelta, TouchPhase};

/// How far a trackpad's fingers move for one level. Lemon tried 16px to 96px in the
/// experiment and kept 40.
const TRACKPAD_STEP: f32 = 40.;
/// Scrolling seen elsewhere this recently is still meant for there when the pointer
/// reaches the meter. AppKit latches a gesture to the view it started in, but gpui hits
/// each event wherever the pointer is.
const LATCH: Duration = Duration::from_millis(180);
/// A pause this long between wheel events starts afresh: a fling's momentum has run
/// out, and fingers moved on a device that sends no gesture phases start a new sum.
const GESTURE_GAP: Duration = Duration::from_millis(220);

/// The trackpad's fingers as the window's wheel events tell them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Fingers {
    /// No gesture with phases is going on: a mouse wheel, or a device that sends none.
    #[default]
    Off,
    /// On the trackpad, in a gesture that began over the meter (`ours`) or elsewhere.
    Down { ours: bool },
    /// Lifted after a gesture: what comes now is the fling's momentum, which macOS
    /// sends with no phase of its own, so gpui gives it as `Moved`.
    Coasting,
}

/// What an event over the meter does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Take {
    /// Nothing; it goes on to whatever is under the meter.
    Pass,
    /// Nothing yet, but it is the meter's.
    Hold,
    /// The meter's: one level up (1) or down (-1).
    Step(isize),
}

/// The wheel as the effort meter follows it, across the whole window.
#[derive(Debug, Default)]
pub(super) struct Wheel {
    fingers: Fingers,
    /// When the window last had a wheel event.
    last: Option<Instant>,
    /// That event, until the meter takes it: one still untaken when the next comes was
    /// somewhere else.
    untaken: Option<Instant>,
    /// When the last event elsewhere came.
    outside: Option<Instant>,
    /// When the meter last let an event pass for coming from elsewhere: a scroll that
    /// keeps coming stays someone else's.
    passed: Option<Instant>,
    /// The scroll going on over the meter is the meter's.
    engaged: bool,
    /// How far a trackpad has moved toward the next level, up for more.
    moved: f32,
}

impl Wheel {
    /// Sees each of the window's wheel events, wherever it lands, before anything
    /// takes it.
    pub(super) fn see(&mut self, phase: TouchPhase, now: Instant) {
        if let Some(at) = self.untaken.take() {
            self.outside = Some(at);
            self.engaged = false;
        }
        if self.last.is_some_and(|last| now - last >= GESTURE_GAP) {
            self.moved = 0.;
            if self.fingers == Fingers::Coasting {
                self.fingers = Fingers::Off;
            }
        }
        self.last = Some(now);
        self.untaken = Some(now);
        match phase {
            TouchPhase::Started => self.fingers = Fingers::Down { ours: false },
            TouchPhase::Ended => self.fingers = Fingers::Coasting,
            // Where a gesture can be called off, no fling follows.
            TouchPhase::Cancelled => self.fingers = Fingers::Off,
            TouchPhase::Moved => {}
        }
    }

    /// An event over the meter, after `see`. `more` is how far it goes toward a higher
    /// level, in its own lines or pixels.
    pub(super) fn take(
        &mut self,
        phase: TouchPhase,
        delta: &ScrollDelta,
        more: f32,
        now: Instant,
    ) -> Take {
        self.untaken = None;
        // A wheel's notches never come from the trackpad, whatever state its fingers were
        // last left in: macOS sends a touch that never moved as begun, then cancelled,
        // and gpui gives the cancel as `Moved`. However far one notch goes, it is one
        // level.
        if let ScrollDelta::Lines(_) = delta {
            return match self.engage(now) {
                false => Take::Pass,
                true if more > 0. => Take::Step(1),
                true if more < 0. => Take::Step(-1),
                true => Take::Hold,
            };
        }
        match (phase, self.fingers) {
            (TouchPhase::Started, Fingers::Down { .. }) => {
                // Fingers put down on the meter make it the meter's gesture, however
                // recently something else scrolled.
                self.fingers = Fingers::Down { ours: true };
                self.engaged = true;
                self.moved = 0.;
                Take::Hold
            }
            (TouchPhase::Ended | TouchPhase::Cancelled, _) => {
                if self.engaged {
                    Take::Hold
                } else {
                    Take::Pass
                }
            }
            (_, Fingers::Coasting | Fingers::Down { ours: false }) => Take::Pass,
            (_, Fingers::Down { ours: true }) => self.add(more),
            (_, Fingers::Off) => match self.engage(now) {
                true => self.add(more),
                false => Take::Pass,
            },
        }
    }

    /// Whether a scroll with no gesture of its own to go by is the meter's: it is,
    /// unless scrolling elsewhere came just before it.
    fn engage(&mut self, now: Instant) -> bool {
        if !self.engaged {
            let recent = |at: Option<Instant>| at.is_some_and(|at| now - at < LATCH);
            if recent(self.outside) || recent(self.passed) {
                self.passed = Some(now);
                return false;
            }
            self.engaged = true;
        }
        true
    }

    /// The meter can't go further that way: what the trackpad had moved toward the
    /// next level is dropped, so turning back answers at once.
    pub(super) fn stop(&mut self) {
        self.moved = 0.;
    }

    /// Adds a trackpad's pixels toward the next level. A turn back starts the sum again,
    /// and one event moves at most one level, however far it goes.
    fn add(&mut self, more: f32) -> Take {
        if more == 0. {
            return Take::Hold;
        }
        if self.moved != 0. && more.signum() != self.moved.signum() {
            self.moved = 0.;
        }
        self.moved += more;
        if self.moved.abs() < TRACKPAD_STEP {
            return Take::Hold;
        }
        let dir = self.moved.signum();
        self.moved = dir * (self.moved.abs() - TRACKPAD_STEP).min(TRACKPAD_STEP - 1.);
        Take::Step(dir as isize)
    }
}

/// How far a scroll goes toward a higher level: fingers up or right, or a wheel turned
/// away from the user, is more, as dragging the slider right is. gpui's deltas are the
/// content's movement, which follows the fingers while macOS's "natural" scrolling is
/// on, so with it off they count the other way.
pub(super) fn toward_more(delta: &ScrollDelta, natural: bool) -> f32 {
    let (x, y) = match delta {
        ScrollDelta::Pixels(delta) => (delta.x.as_f32(), delta.y.as_f32()),
        ScrollDelta::Lines(delta) => (delta.x, delta.y),
    };
    let more = x - y;
    if natural { more } else { -more }
}

/// Whether macOS's "natural" scrolling is on: it is unless the user turned it off.
/// Elsewhere, content moves against the fingers.
pub(super) fn natural_scrolling() -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSUserDefaults, ns_string};
        let defaults = NSUserDefaults::standardUserDefaults();
        let key = ns_string!("com.apple.swipescrolldirection");
        defaults.objectForKey(key).is_none() || defaults.boolForKey(key)
    }
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, px};

    use super::*;

    const LINE: ScrollDelta = ScrollDelta::Lines(gpui_kit::Point { x: 0., y: -1. });

    fn pixels(more: f32) -> ScrollDelta {
        ScrollDelta::Pixels(point(px(0.), px(-more)))
    }

    /// An event over the meter at `ms`.
    fn over(wheel: &mut Wheel, phase: TouchPhase, delta: ScrollDelta, at: Instant) -> Take {
        wheel.see(phase, at);
        wheel.take(phase, &delta, toward_more(&delta, true), at)
    }

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn each_notch_of_a_wheel_is_one_level() {
        let (mut wheel, start) = (Wheel::default(), Instant::now());
        let fast = ScrollDelta::Lines(point(0., -6.));
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, LINE, start),
            Take::Step(1)
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, fast, ms(start, 8)),
            Take::Step(1)
        );
        let back = ScrollDelta::Lines(point(0., 1.));
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, back, ms(start, 16)),
            Take::Step(-1)
        );
    }

    #[test]
    fn a_trackpad_moves_a_level_each_40px_and_never_two_at_once() {
        let (mut wheel, start) = (Wheel::default(), Instant::now());
        assert_eq!(
            over(&mut wheel, TouchPhase::Started, pixels(0.), start),
            Take::Hold
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, pixels(25.), ms(start, 16)),
            Take::Hold
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, pixels(25.), ms(start, 32)),
            Take::Step(1)
        );
        // 10px carried over, and 200px more is still one level.
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, pixels(200.), ms(start, 48)),
            Take::Step(1)
        );
        assert_eq!(wheel.moved, 39.);
    }

    #[test]
    fn turning_back_starts_the_sum_again() {
        let (mut wheel, start) = (Wheel::default(), Instant::now());
        over(&mut wheel, TouchPhase::Started, pixels(0.), start);
        over(&mut wheel, TouchPhase::Moved, pixels(30.), ms(start, 16));
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, pixels(-20.), ms(start, 32)),
            Take::Hold
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, pixels(-20.), ms(start, 48)),
            Take::Step(-1)
        );
    }

    #[test]
    fn a_flings_momentum_moves_nothing() {
        let (mut wheel, start) = (Wheel::default(), Instant::now());
        over(&mut wheel, TouchPhase::Started, pixels(0.), start);
        over(&mut wheel, TouchPhase::Moved, pixels(30.), ms(start, 16));
        assert_eq!(
            over(&mut wheel, TouchPhase::Ended, pixels(0.), ms(start, 32)),
            Take::Hold
        );
        for frame in 3..30 {
            let event = over(
                &mut wheel,
                TouchPhase::Moved,
                pixels(60.),
                ms(start, frame * 16),
            );
            assert_eq!(event, Take::Pass);
        }
        // Fingers down again end the momentum at once.
        assert_eq!(
            over(&mut wheel, TouchPhase::Started, pixels(0.), ms(start, 500)),
            Take::Hold
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, pixels(40.), ms(start, 516)),
            Take::Step(1)
        );
    }

    #[test]
    fn a_gesture_begun_elsewhere_stays_elsewhere() {
        let (mut wheel, start) = (Wheel::default(), Instant::now());
        // Begun over the transcript: nothing on the meter takes these.
        wheel.see(TouchPhase::Started, start);
        wheel.see(TouchPhase::Moved, ms(start, 16));
        for frame in 2..10 {
            let event = over(
                &mut wheel,
                TouchPhase::Moved,
                pixels(60.),
                ms(start, frame * 16),
            );
            assert_eq!(event, Take::Pass);
        }
    }

    #[test]
    fn a_wheel_still_turning_elsewhere_isnt_taken() {
        let (mut wheel, start) = (Wheel::default(), Instant::now());
        wheel.see(TouchPhase::Moved, start);
        // Within 180ms of that, and of each other: still the transcript's.
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, LINE, ms(start, 100)),
            Take::Pass
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, LINE, ms(start, 250)),
            Take::Pass
        );
        // A pause, and the next notch is the meter's.
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, LINE, ms(start, 600)),
            Take::Step(1)
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, LINE, ms(start, 650)),
            Take::Step(1)
        );
    }

    #[test]
    fn a_touch_that_never_moved_leaves_the_wheel_its_notches() {
        let (mut wheel, start) = (Wheel::default(), Instant::now());
        // Resting fingers on the trackpad and lifting them: begun, then a cancel that
        // gpui gives as `Moved`.
        over(&mut wheel, TouchPhase::Started, pixels(0.), start);
        over(&mut wheel, TouchPhase::Moved, pixels(0.), ms(start, 30));
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, LINE, ms(start, 400)),
            Take::Step(1)
        );
        assert_eq!(
            over(&mut wheel, TouchPhase::Moved, LINE, ms(start, 420)),
            Take::Step(1)
        );
    }

    #[test]
    fn natural_scrolling_off_turns_the_direction_round() {
        let up = ScrollDelta::Pixels(point(px(0.), px(-10.)));
        assert_eq!(toward_more(&up, true), 10.);
        assert_eq!(toward_more(&up, false), -10.);
        let right = ScrollDelta::Pixels(point(px(10.), px(0.)));
        assert_eq!(toward_more(&right, true), 10.);
    }
}
