//! Adaptive back-pressure shared by every connection of an upload run (web:
//! "Adaptive back-pressure" in `uploadChunkedFile`). Transient problems
//! (network, timeout, 429, 408, 425, 502-504, Cloudflare 52x) are not failures:
//! they open a shared, jittered, exponentially growing "everybody wait" window
//! (honouring `Retry-After`) and halve the connection count (AIMD); the cap
//! grows back by one after every 6 clean chunks.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// HTTP statuses that mean "slow down", not "this chunk is broken".
pub fn is_transient_status(status: u16) -> bool {
    matches!(status, 429 | 408 | 425 | 502 | 503 | 504) || (520..=524).contains(&status)
}

/// Pause before retry `n` (1-based) of a real failure: 1 s, 2 s, 4 s, ... capped at 16 s.
pub fn retry_delay(retry: u32) -> Duration {
    let ms = 1000u64.saturating_mul(1u64 << retry.saturating_sub(1).min(16));
    Duration::from_millis(ms.min(16_000))
}

#[derive(Debug, Default)]
struct State {
    until: Option<Instant>,
    streak: u32,
    /// `None` = no cap (the web's `Infinity`).
    cap: Option<u32>,
    recovery: u32,
}

#[derive(Debug)]
pub struct Backoff {
    state: Mutex<State>,
    /// Multiplies every computed wait (1.0; tests run faster).
    scale: f64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { state: Mutex::default(), scale: 1.0 }
    }
}

impl Backoff {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_scale(scale: f64) -> Self {
        Self { state: Mutex::default(), scale }
    }

    /// `noteTransientProblem`. `active` = connections currently in use.
    pub fn note_transient(&self, suggested: Option<Duration>, active: u32) {
        self.note_transient_at(Instant::now(), suggested, active, fastrand::f64());
    }

    fn note_transient_at(&self, now: Instant, suggested: Option<Duration>, active: u32, rand01: f64) {
        let mut s = self.state.lock().unwrap();
        if s.until.is_none_or(|u| now >= u) {
            // first failure of a new episode escalates; the others failing at the same moment don't
            s.streak = (s.streak + 1).min(8);
            let base_ms = (500.0 * 2f64.powi(s.streak as i32 - 1)).min(15_000.0);
            let jittered = Duration::from_secs_f64(base_ms * (0.75 + rand01 * 0.5) / 1000.0 * self.scale);
            s.until = Some(now + suggested.unwrap_or_default().mul_f64(self.scale).max(jittered));
            s.cap = Some((active.max(2) / 2).max(1));
            s.recovery = 0;
        } else if let Some(sug) = suggested {
            let sug = sug.mul_f64(self.scale);
            let until = s.until.map_or(now + sug, |u| u.max(now + sug));
            s.until = Some(until);
        }
    }

    /// `noteChunkSuccess`. `target` = the connection count the plan wants.
    pub fn note_success(&self, target: u32) {
        let mut s = self.state.lock().unwrap();
        s.streak = 0;
        if let Some(cap) = s.cap {
            s.recovery += 1;
            if s.recovery >= 6 {
                s.recovery = 0;
                let cap = cap + 1;
                s.cap = if cap >= target { None } else { Some(cap) };
            }
        }
    }

    /// Connection cap from back-pressure (`None` = unlimited).
    pub fn cap(&self) -> Option<u32> {
        self.state.lock().unwrap().cap
    }

    /// Time left in the current "everybody wait" window.
    pub fn remaining(&self) -> Duration {
        let s = self.state.lock().unwrap();
        s.until.map_or(Duration::ZERO, |u| u.saturating_duration_since(Instant::now()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_statuses() {
        for s in [429, 408, 425, 502, 503, 504, 520, 524] {
            assert!(is_transient_status(s), "{s}");
        }
        for s in [400, 403, 404, 500, 501, 525] {
            assert!(!is_transient_status(s), "{s}");
        }
    }

    #[test]
    fn retry_delays() {
        let d: Vec<u64> = (1..=7).map(|r| retry_delay(r).as_millis() as u64).collect();
        assert_eq!(d, vec![1000, 2000, 4000, 8000, 16000, 16000, 16000]);
        assert_eq!(retry_delay(40), Duration::from_millis(16_000));
    }

    #[test]
    fn aimd_and_escalation() {
        let b = Backoff::new();
        let t0 = Instant::now();
        // rand 0.5 -> exactly the base delay
        b.note_transient_at(t0, None, 8, 0.5);
        assert_eq!(b.cap(), Some(4));
        let until1 = b.state.lock().unwrap().until.unwrap();
        assert_eq!(until1 - t0, Duration::from_millis(500));
        // a second worker failing inside the same window does not escalate
        b.note_transient_at(t0 + Duration::from_millis(100), None, 4, 0.5);
        assert_eq!(b.state.lock().unwrap().streak, 1);
        // Retry-After extends the window
        b.note_transient_at(t0 + Duration::from_millis(100), Some(Duration::from_secs(3)), 4, 0.5);
        assert_eq!(b.state.lock().unwrap().until.unwrap() - t0, Duration::from_millis(3100));
        // next episode escalates: 1000 ms
        let t1 = t0 + Duration::from_secs(4);
        b.note_transient_at(t1, None, 4, 0.5);
        assert_eq!(b.state.lock().unwrap().until.unwrap() - t1, Duration::from_millis(1000));
        assert_eq!(b.cap(), Some(2));
        // recovery: +1 per 6 successes until the plan's target is reached
        for _ in 0..6 {
            b.note_success(4);
        }
        assert_eq!(b.cap(), Some(3));
        for _ in 0..6 {
            b.note_success(4);
        }
        assert_eq!(b.cap(), None);
        assert_eq!(b.state.lock().unwrap().streak, 0);
    }

    #[test]
    fn escalation_caps_at_15s() {
        let b = Backoff::new();
        let mut t = Instant::now();
        for _ in 0..12 {
            b.note_transient_at(t, None, 1, 0.5);
            t += Duration::from_secs(20);
        }
        let s = b.state.lock().unwrap();
        assert_eq!(s.streak, 8);
        assert_eq!(s.until.unwrap() - (t - Duration::from_secs(20)), Duration::from_millis(15_000));
        assert_eq!(s.cap, Some(1));
    }
}
