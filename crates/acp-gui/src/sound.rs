//! The effort meter's sounds: a short tick for each level it reaches, higher for a higher
//! level, and a low knock for pushing past either end. They are made here, as WAV data
//! from the experiment's Web Audio recipe, so the app ships no sound files, and played
//! through AppKit's NSSound, only while the system's "Play user interface sound effects"
//! is on. Where there is no AppKit they are silent.

use std::time::Duration;

/// How far apart the ticks of a change of several levels sound, and the meter's marks
/// light up.
pub(crate) const TICK_GAP: Duration = Duration::from_millis(28);

/// The tick for the `level`th of a setting's choices, counting from 0.
pub(crate) fn tick(level: usize) {
    #[cfg(target_os = "macos")]
    mac::play(mac::Sound::Tick(level.min(mac::TICK_STEPS.len() - 1)));
    #[cfg(not(target_os = "macos"))]
    let _ = level;
}

/// The knock for pushing past an end. One is heard at most each 120ms, however fast the
/// wheel keeps pushing.
pub(crate) fn knock() {
    #[cfg(target_os = "macos")]
    mac::play(mac::Sound::Knock);
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::f32::consts::TAU;
    use std::time::{Duration, Instant};

    use objc2::AnyThread as _;
    use objc2::rc::Retained;
    use objc2_app_kit::NSSound;
    use objc2_foundation::{NSData, NSUserDefaults, ns_string};

    /// The ticks climb a major pentatonic scale from 620Hz, a step a level, in semitones.
    pub(super) const TICK_STEPS: [f32; 8] = [0., 2., 4., 7., 9., 12., 14., 16.];
    const TICK_BASE: f32 = 620.;
    const KNOCK_GAP: Duration = Duration::from_millis(120);
    const RATE: u32 = 44_100;

    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    pub(super) enum Sound {
        Tick(usize),
        Knock,
    }

    thread_local! {
        /// Each sound made once, as it is first played. NSSound is used on the main
        /// thread only, where gpui runs the app.
        static SOUNDS: RefCell<HashMap<Sound, Retained<NSSound>>> = RefCell::default();
        static LAST_KNOCK: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    pub(super) fn play(sound: Sound) {
        if sound == Sound::Knock {
            let now = Instant::now();
            if LAST_KNOCK.get().is_some_and(|last| now - last < KNOCK_GAP) {
                return;
            }
            LAST_KNOCK.set(Some(now));
        }
        if !ui_sounds_on() {
            return;
        }
        SOUNDS.with_borrow_mut(|sounds| {
            let made = match sounds.get(&sound) {
                Some(made) => made.clone(),
                None => {
                    let samples = match sound {
                        Sound::Tick(step) => tick(TICK_BASE * (TICK_STEPS[step] / 12.).exp2()),
                        Sound::Knock => knock(),
                    };
                    let data = NSData::with_bytes(&wav(&samples));
                    let Some(made) = NSSound::initWithData(NSSound::alloc(), &data) else {
                        return;
                    };
                    sounds.insert(sound, made.clone());
                    made
                }
            };
            // A sound still playing doesn't start again until it is stopped.
            made.stop();
            made.play();
        });
    }

    /// The system's "Play user interface sound effects", on unless turned off. macOS
    /// keeps it with its own sounds' settings, which change as it does.
    fn ui_sounds_on() -> bool {
        let Some(defaults) = NSUserDefaults::initWithSuiteName(
            NSUserDefaults::alloc(),
            Some(ns_string!("com.apple.systemsound")),
        ) else {
            return true;
        };
        let key = ns_string!("com.apple.sound.uiaudio.enabled");
        defaults.objectForKey(key).is_none() || defaults.boolForKey(key)
    }

    /// A tick at `pitch`: a triangle wave falling 10% in its first 60ms, rising in 3ms
    /// and dying away by 75ms.
    fn tick(pitch: f32) -> Vec<f32> {
        render(
            0.09,
            |t| pitch * glide(1., 0.9, t / 0.06),
            0.003,
            0.075,
            triangle,
        )
    }

    /// The knock: a sine falling from 170Hz to 90Hz over 90ms, rising in 4ms and dying
    /// away by 130ms.
    fn knock() -> Vec<f32> {
        render(
            0.14,
            |t| 170. * glide(1., 90. / 170., t / 0.09),
            0.004,
            0.13,
            |phase| (phase * TAU).sin(),
        )
    }

    /// Samples of a wave at `pitch(t)` Hz, whose loudness rises to `peak` by `rise`
    /// seconds and falls back by `end`, both exponentially as Web Audio's ramps do, then
    /// through Web Audio's 4.8kHz lowpass and 0.9 gain.
    fn render(
        peak: f32,
        pitch: impl Fn(f32) -> f32,
        rise: f32,
        end: f32,
        wave: impl Fn(f32) -> f32,
    ) -> Vec<f32> {
        const QUIET: f32 = 0.0001;
        let lowpass = 1. - (-TAU * 4800. / RATE as f32).exp();
        let (mut phase, mut out) = (0f32, 0f32);
        // A little silence after the end, for the lowpass to settle.
        let count = ((end + 0.015) * RATE as f32) as usize;
        (0..count)
            .map(|n| {
                let t = n as f32 / RATE as f32;
                let gain = if t < rise {
                    glide(QUIET, peak, t / rise)
                } else if t < end {
                    glide(peak, QUIET, (t - rise) / (end - rise))
                } else {
                    0.
                };
                phase = (phase + pitch(t) / RATE as f32).fract();
                out += lowpass * (wave(phase) * gain - out);
                out * 0.9
            })
            .collect()
    }

    /// From `from` toward `to` exponentially, `progress` of the way, staying at `to`
    /// after.
    fn glide(from: f32, to: f32, progress: f32) -> f32 {
        from * (to / from).powf(progress.min(1.))
    }

    /// A triangle wave over one turn of `phase`, from -1 up to 1 and back.
    fn triangle(phase: f32) -> f32 {
        1. - 4. * (phase - 0.5).abs()
    }

    /// 16-bit mono PCM in a WAV file, as NSSound reads it.
    fn wav(samples: &[f32]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        // PCM, one channel.
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&RATE.to_le_bytes());
        bytes.extend_from_slice(&(RATE * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            let sample = (sample.clamp(-1., 1.) * i16::MAX as f32) as i16;
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }
}
