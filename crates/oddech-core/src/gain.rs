//! The attenuation layer: turns active breath regions into a per-sample
//! gain envelope.
//!
//! This is the single source of truth for how ducking sounds. Both the
//! real-time playback path and the offline export call
//! [`GainEnvelope::render`], so exported audio is identical sample-for-sample
//! to what was heard.
//!
//! Shape: a region is attenuated at its full depth over `[start, end)`; the
//! gain ramps down over `fade` samples *before* `start` and back up over
//! `fade` samples *after* `end`, using a raised-cosine curve. Placing the
//! ramps outside the region keeps the whole marked breath at full depth.
//! Overlapping spans combine by taking the minimum gain.

use serde::{Deserialize, Serialize};

use crate::region::{Gate, Region};
use crate::time::secs_to_sample;

/// Track-wide attenuation defaults.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DuckParams {
    /// Attenuation in dB (≤ 0). `f32::NEG_INFINITY` = full mute.
    #[serde(with = "db_serde")]
    pub gain_db: f32,
    /// Edge fade length in milliseconds.
    pub fade_ms: f32,
}

impl Default for DuckParams {
    fn default() -> Self {
        Self { gain_db: DEFAULT_GAIN_DB, fade_ms: DEFAULT_FADE_MS }
    }
}

pub const DEFAULT_GAIN_DB: f32 = -18.0;
pub const DEFAULT_FADE_MS: f32 = 15.0;
/// Slider floor: values at or below this are treated as full mute.
pub const MUTE_FLOOR_DB: f32 = -60.0;

/// Converts dB to a linear factor; anything at or below [`MUTE_FLOOR_DB`]
/// is exact silence.
#[inline]
pub fn db_to_linear(db: f32) -> f32 {
    if db <= MUTE_FLOOR_DB {
        0.0
    } else {
        10f32.powf(db.min(0.0) / 20.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Span {
    /// First fully attenuated sample.
    start: i64,
    /// One past the last fully attenuated sample.
    end: i64,
    fade: i64,
    gain: f32,
}

impl Span {
    #[inline]
    fn outer_start(&self) -> i64 {
        self.start - self.fade
    }

    #[inline]
    fn outer_end(&self) -> i64 {
        self.end + self.fade
    }

    /// Gain at sample `n` (1.0 outside the span's influence).
    #[inline]
    fn gain_at(&self, n: i64) -> f32 {
        if n >= self.start && n < self.end {
            self.gain
        } else if n < self.start && n >= self.outer_start() {
            // Ramp down: position 0 at outer start, 1 at start.
            let x = (n - self.outer_start()) as f32 / self.fade as f32;
            1.0 + (self.gain - 1.0) * raised_cosine(x)
        } else if n >= self.end && n < self.outer_end() {
            // Ramp up: exact mirror image of the ramp down, so the k-th
            // sample after `end` has the same gain as the k-th before `start`.
            let x = (self.outer_end() - n - 1) as f32 / self.fade as f32;
            1.0 + (self.gain - 1.0) * raised_cosine(x)
        } else {
            1.0
        }
    }
}

#[inline]
fn raised_cosine(x: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::PI * x.clamp(0.0, 1.0)).cos()
}

/// Immutable, precomputed envelope for one track at one sample rate.
#[derive(Debug, Clone, Default)]
pub struct GainEnvelope {
    /// Sorted by `outer_start`.
    spans: Vec<Span>,
    /// `max_outer_end[i]` = max of `outer_end` over `spans[..=i]`; lets us
    /// binary-search the first span that can affect a given sample.
    max_outer_end: Vec<i64>,
}

impl GainEnvelope {
    /// Builds the envelope from the regions of one track.
    ///
    /// Regions that are not active breaths under `gate` are ignored.
    pub fn build<'a>(
        regions: impl IntoIterator<Item = &'a Region>,
        params: &DuckParams,
        gate: &Gate,
        rate: u32,
    ) -> Self {
        let mut spans: Vec<Span> = regions
            .into_iter()
            .filter(|r| r.is_active_breath(gate))
            .filter_map(|r| {
                let gain = db_to_linear(r.gain_db.unwrap_or(params.gain_db));
                let fade_ms = r.fade_ms.unwrap_or(params.fade_ms).max(0.0);
                let start = secs_to_sample(r.start, rate);
                let end = secs_to_sample(r.end, rate);
                (end > start && gain < 1.0).then(|| Span {
                    start,
                    end,
                    fade: secs_to_sample(f64::from(fade_ms) / 1000.0, rate),
                    gain,
                })
            })
            .collect();
        spans.sort_by_key(|s| (s.outer_start(), s.end));
        let mut running = i64::MIN;
        let max_outer_end = spans
            .iter()
            .map(|s| {
                running = running.max(s.outer_end());
                running
            })
            .collect();
        Self { spans, max_outer_end }
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Index of the first span that may influence sample `n` or later.
    fn first_relevant(&self, n: i64) -> usize {
        self.max_outer_end.partition_point(|&e| e <= n)
    }

    pub fn gain_at(&self, n: i64) -> f32 {
        let mut g = 1.0f32;
        for s in &self.spans[self.first_relevant(n)..] {
            if s.outer_start() > n {
                break;
            }
            g = g.min(s.gain_at(n));
        }
        g
    }

    /// Writes the gain for samples `start .. start + out.len()` into `out`.
    pub fn render(&self, start: i64, out: &mut [f32]) {
        out.fill(1.0);
        let end = start + out.len() as i64;
        for s in &self.spans[self.first_relevant(start)..] {
            if s.outer_start() >= end {
                break;
            }
            let lo = s.outer_start().max(start);
            let hi = s.outer_end().min(end);
            for n in lo..hi {
                let slot = &mut out[(n - start) as usize];
                *slot = slot.min(s.gain_at(n));
            }
        }
    }

    /// Multiplies `samples` (one channel, starting at sample `start`) by the
    /// envelope in place. `scratch` avoids allocation on hot paths.
    pub fn apply(&self, start: i64, samples: &mut [f32], scratch: &mut Vec<f32>) {
        if self.is_unity(start, start + samples.len() as i64) {
            return;
        }
        scratch.resize(samples.len(), 1.0);
        self.render(start, scratch);
        for (x, g) in samples.iter_mut().zip(scratch.iter()) {
            *x *= *g;
        }
    }

    /// True if no span influences `[start, end)`.
    pub fn is_unity(&self, start: i64, end: i64) -> bool {
        self.spans[self.first_relevant(start)..]
            .first()
            .is_none_or(|s| s.outer_start() >= end)
    }

    /// Minimum gain over `[start, end)`; used to draw the "after" waveform
    /// at zoom levels where one pixel covers many samples.
    pub fn min_gain_in(&self, start: i64, end: i64) -> f32 {
        let mut g = 1.0f32;
        for s in &self.spans[self.first_relevant(start)..] {
            if s.outer_start() >= end {
                break;
            }
            if s.start < end && s.end > start {
                g = g.min(s.gain);
            } else {
                // Only a ramp overlaps: its extreme inside the window is at
                // the window edge closest to the core.
                let probe = if s.outer_end() > start && s.end <= start { start } else { end - 1 };
                g = g.min(s.gain_at(probe));
            }
        }
        g
    }
}

/// Serde for dB values where `-inf` (full mute) is stored as `"mute"`.
pub mod db_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
        if v.is_finite() { s.serialize_f32(*v) } else { s.serialize_str("mute") }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Db(f32),
            Word(String),
        }
        match Repr::deserialize(d)? {
            Repr::Db(v) => Ok(v),
            Repr::Word(w) if w == "mute" => Ok(f32::NEG_INFINITY),
            Repr::Word(w) => Err(serde::de::Error::custom(format!("invalid gain {w:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::{RegionId, RegionKind, RegionSource};
    use crate::track::TrackId;

    const RATE: u32 = 1000;

    fn breath(start: f64, end: f64) -> Region {
        Region {
            id: RegionId(0),
            track: TrackId(0),
            start,
            end,
            kind: RegionKind::Breath,
            source: RegionSource::User,
            confidence: None,
            gain_db: None,
            fade_ms: None,
            silence_db: Vec::new(),
        }
    }

    fn params(db: f32, fade_ms: f32) -> DuckParams {
        DuckParams { gain_db: db, fade_ms }
    }

    #[test]
    fn full_depth_inside_unity_outside() {
        let env = GainEnvelope::build(&[breath(1.0, 2.0)], &params(-20.0, 10.0), &Gate { threshold: 0.5, discrimination: Default::default() }, RATE);
        assert!((env.gain_at(1500) - 0.1).abs() < 1e-6);
        assert!((env.gain_at(1000) - 0.1).abs() < 1e-6);
        assert!((env.gain_at(1999) - 0.1).abs() < 1e-6);
        assert_eq!(env.gain_at(989), 1.0);
        assert_eq!(env.gain_at(2010), 1.0);
        // Ramps are monotonic.
        let down: Vec<f32> = (990..1000).map(|n| env.gain_at(n)).collect();
        assert!(down.windows(2).all(|w| w[1] <= w[0]));
        let up: Vec<f32> = (2000..2010).map(|n| env.gain_at(n)).collect();
        assert!(up.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn render_matches_pointwise_evaluation() {
        let regions = [breath(0.10, 0.20), breath(0.195, 0.30), breath(0.5, 0.52)];
        let env = GainEnvelope::build(&regions, &params(-18.0, 15.0), &Gate { threshold: 0.5, discrimination: Default::default() }, RATE);
        let mut out = vec![0.0; 700];
        env.render(0, &mut out);
        for (n, g) in out.iter().enumerate() {
            assert_eq!(*g, env.gain_at(n as i64), "sample {n}");
        }
        // Rendering in arbitrary blocks gives the same result.
        let mut blocks = Vec::new();
        let mut pos = 0;
        for len in [1usize, 7, 64, 3, 250, 375] {
            let mut b = vec![0.0; len];
            env.render(pos, &mut b);
            blocks.extend(b);
            pos += len as i64;
        }
        assert_eq!(blocks, out);
    }

    #[test]
    fn overlapping_spans_take_minimum() {
        let mut a = breath(1.0, 2.0);
        a.gain_db = Some(-6.0);
        let mut b = breath(1.5, 2.5);
        b.gain_db = Some(-40.0);
        let env = GainEnvelope::build(&[a, b], &params(-18.0, 0.0), &Gate { threshold: 0.5, discrimination: Default::default() }, RATE);
        assert!((env.gain_at(1200) - db_to_linear(-6.0)).abs() < 1e-6);
        assert!((env.gain_at(1700) - db_to_linear(-40.0)).abs() < 1e-6);
    }

    #[test]
    fn mute_and_zero_db() {
        let env = GainEnvelope::build(&[breath(1.0, 2.0)], &params(f32::NEG_INFINITY, 0.0), &Gate { threshold: 0.5, discrimination: Default::default() }, RATE);
        assert_eq!(env.gain_at(1500), 0.0);
        let env = GainEnvelope::build(&[breath(1.0, 2.0)], &params(0.0, 10.0), &Gate { threshold: 0.5, discrimination: Default::default() }, RATE);
        assert!(env.is_empty());
    }

    #[test]
    fn inactive_regions_are_ignored() {
        let mut r = breath(1.0, 2.0);
        r.source = RegionSource::Model;
        r.confidence = Some(0.3);
        let env = GainEnvelope::build(&[r], &params(-18.0, 0.0), &Gate { threshold: 0.5, discrimination: Default::default() }, RATE);
        assert!(env.is_empty());
    }

    #[test]
    fn unity_and_min_gain_queries() {
        let env = GainEnvelope::build(&[breath(1.0, 2.0)], &params(-20.0, 10.0), &Gate { threshold: 0.5, discrimination: Default::default() }, RATE);
        assert!(env.is_unity(0, 990));
        assert!(!env.is_unity(0, 991));
        assert!(env.is_unity(2010, 5000));
        assert!((env.min_gain_in(0, 5000) - 0.1).abs() < 1e-6);
        assert_eq!(env.min_gain_in(0, 900), 1.0);
        let ramp = env.min_gain_in(2005, 2020);
        assert!(ramp > 0.1 && ramp < 1.0);
        assert_eq!(ramp, env.gain_at(2005));
    }
}
