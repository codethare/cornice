//! Easing and tweening. Notification cards can have independent motions, so this is a small reusable value type rather than a global animation owner.

use std::time::{Duration, Instant};

/// Apple's motion language is springs rather than easing curves (`spring(response:dampingFraction:)`, and WWDC23
/// "Animate with springs" recommends them for every state change): the movement starts fast and lands softly, with a
/// little overshoot when something *opens* and none when it *closes*.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    /// Something opening: ζ = 0.7, about a 4% overshoot — the "bouncy but calm" preset (`.bouncy`).
    Spring,
    /// Something closing: critically damped (ζ = 1), the untuned ``.smooth`` preset — no overshoot.
    Smooth,
}

impl Easing {
    /// Input is clamped only: the endpoints must land on 0.0 / 1.0, which clamping the output would break.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Spring => spring(t, 0.7),
            Easing::Smooth => spring(t, 1.0),
        }
    }
}

/// Damped harmonic oscillator over the tween's normalised time. ω = 8 puts the spring's natural period inside the
/// configured duration (Apple's `response` is a period, not a settle time): the envelope is down to e⁻⁵·⁶ by the end,
/// so `enter_ms` still means what it says — a spring that settled in a fraction of the window would make the setting
/// decorative.
fn spring(t: f32, damping: f32) -> f32 {
    let w = 8.0f32;
    if damping >= 1.0 {
        return 1.0 - (1.0 + w * t) * (-w * t).exp();
    }
    let wd = w * (1.0 - damping * damping).sqrt();
    1.0 - (-damping * w * t).exp() * ((wd * t).cos() + (damping * w / wd) * (wd * t).sin())
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    pub start: Instant,
    pub dur: Duration,
}

impl Tween {
    pub fn new(start: Instant, dur_ms: u64) -> Self {
        Self { start, dur: Duration::from_millis(dur_ms) }
    }

    /// Linear progress 0..1; the caller wraps it in `Easing::apply` when it wants easing.
    pub fn progress(&self, now: Instant) -> f32 {
        // Zero duration means "complete immediately", avoiding a division by zero.
        if self.dur.is_zero() {
            return 1.0;
        }
        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
        (elapsed / self.dur.as_secs_f32()).clamp(0.0, 1.0)
    }

    pub fn is_done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn springs_open_with_a_small_overshoot_and_close_without_one() {
        // Both ends of the tween are what they say: nothing at 0, and the target reached by 1 (the tiny remainder
        // e⁻¹² is invisible and the animation drops the tween at that point anyway).
        for e in [Easing::Spring, Easing::Smooth] {
            assert_eq!(e.apply(0.0), 0.0);
            assert!((e.apply(1.0) - 1.0).abs() < 1e-2, "{e:?} must land on the target: {}", e.apply(1.0));
            assert!(e.apply(0.5) > 0.5, "a spring starts fast: {e:?} at the midpoint is {}", e.apply(0.5));
        }
        // It also has to be mid-flight early in the window, or a long `enter_ms` would be ignored (the control check
        // in `scripts/live/run.sh` observes a new card 500 ms into a 1500 ms animation).
        assert!(Easing::Spring.apply(1.0 / 3.0) < 0.95, "a third of the way in, the spring is still moving: {}", Easing::Spring.apply(1.0 / 3.0));
        let peak = (0..=100).map(|i| Easing::Spring.apply(i as f32 / 100.0)).fold(0.0f32, f32::max);
        assert!(peak > 1.0 && peak < 1.10, "the opening overshoots a little, not wildly: {peak}");
        assert!((0..=100).all(|i| Easing::Smooth.apply(i as f32 / 100.0) <= 1.0), "the closing motion must not overshoot");
    }

    #[test]
    fn tween_progress_is_clamped_and_monotonic() {
        let t0 = Instant::now();
        let tw = Tween::new(t0, 200);
        assert_eq!(tw.progress(t0), 0.0);
        assert_eq!(tw.progress(t0 + Duration::from_millis(100)), 0.5);
        assert_eq!(tw.progress(t0 + Duration::from_millis(200)), 1.0);
        assert_eq!(tw.progress(t0 + Duration::from_secs(10)), 1.0, "past the end should clamp to 1");
        assert!(!tw.is_done(t0 + Duration::from_millis(199)));
        assert!(tw.is_done(t0 + Duration::from_millis(200)));
    }

    #[test]
    fn zero_duration_is_instant() {
        let t0 = Instant::now();
        let tw = Tween::new(t0, 0);
        assert_eq!(tw.progress(t0), 1.0);
        assert!(tw.is_done(t0));
    }
}
