use std::time::{Duration, Instant};

const SETTLE_TOLERANCE_MS: u32 = 1_000;
const SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
const DRIFT_IGNORE_MS: u32 = 250;

/// Smooths display ticks and briefly protects a new context from older reports.
/// Player separately rejects reports sampled before a seek or restore.
pub(super) struct PlaybackClock {
    accepted_ms: u32,
    display_anchor_ms: u32,
    display_anchor_at: Instant,
    playing: bool,
    settling_until: Option<Instant>,
}

impl PlaybackClock {
    pub(super) fn new() -> Self {
        Self {
            accepted_ms: 0,
            display_anchor_ms: 0,
            display_anchor_at: Instant::now(),
            playing: false,
            settling_until: None,
        }
    }

    pub(super) fn accepted_ms(&self) -> u32 {
        self.accepted_ms
    }

    pub(super) fn display_position_ms(&self, now: Instant) -> u32 {
        if !self.playing {
            return self.accepted_ms;
        }
        let elapsed = now
            .saturating_duration_since(self.display_anchor_at)
            .as_millis();
        self.display_anchor_ms
            .saturating_add(u32::try_from(elapsed).unwrap_or(u32::MAX))
    }

    pub(super) fn set_playing(&mut self, playing: bool, now: Instant) {
        if self.playing != playing {
            self.display_anchor_ms = self.accepted_ms;
            self.display_anchor_at = now;
            self.playing = playing;
        }
    }

    pub(super) fn reset(&mut self, position_ms: u32, now: Instant) {
        self.accepted_ms = position_ms;
        self.display_anchor_ms = position_ms;
        self.display_anchor_at = now;
        self.settling_until = None;
    }

    pub(super) fn seek(&mut self, position_ms: u32, now: Instant) {
        self.reset(position_ms, now);
        self.settling_until = Some(now + SETTLE_TIMEOUT);
    }

    pub(super) fn report(&mut self, position_ms: u32, now: Instant, after_request: bool) -> bool {
        if after_request {
            // This subscription started after the request was processed. A
            // different position is authoritative even when a seek failed.
            self.settling_until = None;
        }
        let displayed_ms = self.display_position_ms(now);
        if let Some(until) = self.settling_until {
            if now < until && position_ms.abs_diff(displayed_ms) > SETTLE_TOLERANCE_MS {
                return false;
            }
            // Keep the guard after an agreeing report: an older report can
            // still be in flight behind that confirmation.
            if now >= until {
                self.settling_until = None;
            }
        }
        self.accepted_ms = position_ms;
        // A close report may leave the display anchor alone, but its accepted
        // position must still become the value used by saves and SMTC.
        if position_ms.abs_diff(displayed_ms) >= DRIFT_IGNORE_MS {
            self.display_anchor_ms = position_ms;
            self.display_anchor_at = now;
        }
        true
    }
}
