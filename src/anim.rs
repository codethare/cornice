//! Easing and tweening. There is exactly one animation (the notification island entering and leaving), so there is no general keyframe system.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    OutCubic,
    InOutCubic,
}

impl Easing {
    /// Input is clamped only: the endpoints must land exactly on 0.0 / 1.0, which clamping the output would break.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::OutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::InOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
        }
    }
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
    fn easing_endpoints() {
        assert_eq!(Easing::OutCubic.apply(0.0), 0.0);
        assert_eq!(Easing::OutCubic.apply(1.0), 1.0);
        assert_eq!(Easing::InOutCubic.apply(0.0), 0.0);
        assert_eq!(Easing::InOutCubic.apply(1.0), 1.0);
        // Ease-out: past the midpoint, more than half of the progress should be done
        assert!(Easing::OutCubic.apply(0.5) > 0.5);
        assert!((Easing::OutCubic.apply(0.5) - 0.875).abs() < 1e-6);
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
