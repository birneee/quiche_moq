//! Buffer-level → playback-speed-multiplier logic, shared by every MoQ
//! subscriber that presents timed media on its own clock (fMP4 file/pipe
//! output in `moq_cli`, `AudioContext` scheduling in the browser client).
//!
//! Pure logic, no I/O and no OS/browser APIs, so it works identically on
//! native and `wasm32-unknown-unknown`.
//!
//! Nudges the presented media clock by a few percent based on how much is
//! buffered ahead of the consumer:
//! - buffer high (well ahead of where the consumer is) -> speed up a
//!   little, draining the backlog before it turns into unbounded latency
//!   drift.
//! - buffer empty or negative (starved / already behind) -> slow down a
//!   little, so the presented timeline doesn't keep outrunning newly
//!   arriving data (avoids stutter/underrun) once more does arrive.
//! - buffer at a healthy level -> no change.
//!
//! This only ever nudges by a few percent (imperceptible) — it's a jitter
//! smoother, not a fast-forward/rewind control. Callers apply the returned
//! multiplier to whatever they use to advance their own presentation clock
//! (an fMP4 fragment duration, a scheduled-audio time delta, ...).

/// One step of the backlog-speed curve: `speed` applies when the buffer
/// level is greater than `above`. A curve needs a band with `above:
/// f32::NEG_INFINITY` to have a defined speed for every level; without one,
/// levels below the lowest band fall back to a neutral `1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub above: f32,
    pub speed: f32,
}

/// A speed curve over an arbitrary number of backlog bands.
#[derive(Debug, Clone, Copy)]
pub struct PacingThresholds<'a> {
    /// Bands in any order; the one with the highest `above` that `level`
    /// exceeds wins.
    pub bands: &'a [Band],
}

impl PacingThresholds<'_> {
    /// `moq_cli`'s original fd-write-backlog pacing. Buffer unit: number of
    /// complete objects already cached ahead of the one currently being
    /// written to the sink.
    pub const FRAGMENT_BACKLOG: PacingThresholds<'static> = PacingThresholds {
        bands: &[
            Band { above: 10.0, speed: 0.90 },
            Band { above: 4.0, speed: 0.95 },
            Band { above: 0.0, speed: 1.0 },
            Band { above: f32::NEG_INFINITY, speed: 1.05 },
        ],
    };

    /// Speed multiplier for the given buffer level. `1.0` if no band's
    /// `above` is exceeded.
    pub fn speed_for(&self, level: f32) -> f32 {
        self.bands
            .iter()
            .filter(|b| level > b.above)
            .max_by(|a, b| a.above.total_cmp(&b.above))
            .map(|b| b.speed)
            .unwrap_or(1.0)
    }

    /// Scales `nominal` (a duration, in whatever unit the caller uses) by
    /// the speed multiplier for `level`.
    pub fn scale(&self, nominal: f32, level: f32) -> f32 {
        nominal * self.speed_for(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_original_fragment_backlog_bands() {
        let t = PacingThresholds::FRAGMENT_BACKLOG;
        assert_eq!(t.speed_for(11.0), 0.90);
        assert_eq!(t.speed_for(5.0), 0.95);
        assert_eq!(t.speed_for(0.0), 1.05); // starved: 0 is not > 0.0
        assert_eq!(t.speed_for(2.0), 1.0);
        assert_eq!(t.speed_for(4.0), 1.0); // boundary: not > 4.0's own band, falls to the 0.0 band
        assert_eq!(t.speed_for(10.0), 0.95); // boundary: not > 10.0's own band, falls to the 4.0 band
    }

    #[test]
    fn negative_level_falls_to_floor_band() {
        let t = PacingThresholds::FRAGMENT_BACKLOG;
        assert_eq!(t.speed_for(-5.0), 1.05);
    }

    #[test]
    fn scale_applies_multiplier() {
        let t = PacingThresholds::FRAGMENT_BACKLOG;
        assert_eq!(t.scale(1000.0, 11.0), 900.0);
    }

    #[test]
    fn no_matching_band_defaults_to_neutral() {
        let t = PacingThresholds { bands: &[Band { above: 10.0, speed: 0.5 }] };
        assert_eq!(t.speed_for(1.0), 1.0);
    }

    #[test]
    fn supports_arbitrary_band_count() {
        let t = PacingThresholds {
            bands: &[
                Band { above: 20.0, speed: 0.80 },
                Band { above: 10.0, speed: 0.90 },
                Band { above: 4.0, speed: 0.95 },
                Band { above: f32::NEG_INFINITY, speed: 1.05 },
            ],
        };
        assert_eq!(t.speed_for(25.0), 0.80);
        assert_eq!(t.speed_for(15.0), 0.90);
        assert_eq!(t.speed_for(6.0), 0.95);
        assert_eq!(t.speed_for(1.0), 1.05);
    }
}