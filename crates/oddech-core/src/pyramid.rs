//! Min/max waveform pyramid ("mipmaps" for audio).
//!
//! Level 0 stores the min and max of every [`BASE_BUCKET`] samples; each
//! following level merges [`LEVEL_FACTOR`] buckets of the previous one. To
//! draw a column that spans `spp` samples we pick the coarsest level whose
//! bucket still fits inside `spp`, so every column touches only a handful of
//! buckets regardless of zoom. Below `BASE_BUCKET` samples per pixel the
//! caller supplies raw samples instead.
//!
//! Values are quantized to `i16`, rounding outward (min down, max up), so
//! the stored envelope never under-reports the signal. This halves memory
//! at no visible cost: one LSB is far below one pixel.

use std::io::{self, Read, Write};

use bytemuck::{Pod, Zeroable};

pub const BASE_BUCKET: usize = 64;
pub const LEVEL_FACTOR: usize = 4;

/// Quantized min/max pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
#[repr(C)]
pub struct QMinMax {
    pub min: i16,
    pub max: i16,
}

impl QMinMax {
    const EMPTY: Self = Self { min: i16::MAX, max: i16::MIN };

    #[inline]
    fn merge(self, o: Self) -> Self {
        Self { min: self.min.min(o.min), max: self.max.max(o.max) }
    }

    #[inline]
    fn to_f32(self) -> MinMax {
        MinMax { min: f32::from(self.min) / 32767.0, max: f32::from(self.max) / 32767.0 }
    }
}

#[inline]
fn quantize_down(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).floor() as i16
}

#[inline]
fn quantize_up(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).ceil() as i16
}

/// Min/max of one display column in linear amplitude.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinMax {
    pub min: f32,
    pub max: f32,
}

impl MinMax {
    pub const EMPTY: Self = Self { min: f32::INFINITY, max: f32::NEG_INFINITY };

    pub fn is_empty(&self) -> bool {
        self.min > self.max
    }

    #[inline]
    pub fn include(&mut self, x: f32) {
        self.min = self.min.min(x);
        self.max = self.max.max(x);
    }

    #[inline]
    pub fn merge(&mut self, o: MinMax) {
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }

    pub fn scaled(self, g: f32) -> Self {
        Self { min: self.min * g, max: self.max * g }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WaveformPyramid {
    levels: Vec<Vec<QMinMax>>,
    len_samples: u64,
}

impl WaveformPyramid {
    pub fn len_samples(&self) -> u64 {
        self.len_samples
    }

    pub fn memory_bytes(&self) -> usize {
        self.levels.iter().map(|l| l.len() * size_of::<QMinMax>()).sum()
    }

    fn bucket_size(level: usize) -> usize {
        BASE_BUCKET * LEVEL_FACTOR.pow(level as u32)
    }

    /// Picks the coarsest level whose bucket is not larger than `spp`.
    /// Returns `None` when `spp < BASE_BUCKET` (raw samples needed).
    pub fn level_for(&self, spp: f64) -> Option<usize> {
        if spp < BASE_BUCKET as f64 || self.levels.is_empty() {
            return None;
        }
        let mut level = 0;
        while level + 1 < self.levels.len() && Self::bucket_size(level + 1) as f64 <= spp {
            level += 1;
        }
        Some(level)
    }

    /// Fills `out` with one min/max per column. Column `c` covers samples
    /// `[first + c·spp, first + (c+1)·spp)`. Columns outside the signal are
    /// [`MinMax::EMPTY`]. Requires `spp >= BASE_BUCKET`; use
    /// [`columns_from_raw`] otherwise.
    pub fn columns(&self, first: f64, spp: f64, cols: usize, out: &mut Vec<MinMax>) {
        out.clear();
        let Some(level) = self.level_for(spp) else {
            out.resize(cols, MinMax::EMPTY);
            return;
        };
        let data = &self.levels[level];
        let bucket = Self::bucket_size(level) as f64;
        for c in 0..cols {
            let s0 = first + c as f64 * spp;
            let s1 = s0 + spp;
            let b0 = (s0 / bucket).floor().max(0.0) as usize;
            let b1 = ((s1 / bucket).ceil().max(0.0) as usize).min(data.len());
            let mut mm = QMinMax::EMPTY;
            for q in data.get(b0..b1).unwrap_or(&[]) {
                mm = mm.merge(*q);
            }
            out.push(if mm.min > mm.max { MinMax::EMPTY } else { mm.to_f32() });
        }
    }

    const MAGIC: &'static [u8; 8] = b"ODWPYR01";

    pub fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(Self::MAGIC)?;
        w.write_all(&self.len_samples.to_le_bytes())?;
        w.write_all(&(self.levels.len() as u32).to_le_bytes())?;
        for level in &self.levels {
            w.write_all(&(level.len() as u64).to_le_bytes())?;
            w.write_all(bytemuck::cast_slice(level))?;
        }
        Ok(())
    }

    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_owned());
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != Self::MAGIC {
            return Err(bad("not a waveform pyramid"));
        }
        let mut u64b = [0u8; 8];
        let mut u32b = [0u8; 4];
        r.read_exact(&mut u64b)?;
        let len_samples = u64::from_le_bytes(u64b);
        r.read_exact(&mut u32b)?;
        let n_levels = u32::from_le_bytes(u32b) as usize;
        if n_levels > 32 {
            return Err(bad("too many pyramid levels"));
        }
        let mut levels = Vec::with_capacity(n_levels);
        for _ in 0..n_levels {
            r.read_exact(&mut u64b)?;
            let n = u64::from_le_bytes(u64b) as usize;
            if n as u64 > len_samples / BASE_BUCKET as u64 + 2 {
                return Err(bad("pyramid level larger than signal"));
            }
            let mut level = vec![QMinMax::EMPTY; n];
            r.read_exact(bytemuck::cast_slice_mut(&mut level))?;
            levels.push(level);
        }
        Ok(Self { levels, len_samples })
    }
}

/// Computes column min/max directly from samples; used when zoomed in past
/// the pyramid's base resolution. `samples[0]` is at absolute sample
/// `samples_start`.
pub fn columns_from_raw(
    samples: &[f32],
    samples_start: i64,
    first: f64,
    spp: f64,
    cols: usize,
    out: &mut Vec<MinMax>,
) {
    out.clear();
    let len = samples.len() as i64;
    for c in 0..cols {
        let s0 = first + c as f64 * spp;
        let s1 = s0 + spp;
        // Include the neighbouring sample on each side so that, when a
        // column holds fewer than one sample, adjacent columns still join
        // into a continuous trace.
        let i0 = (s0.floor() as i64 - samples_start).clamp(0, len);
        let i1 = (s1.ceil() as i64 - samples_start + 1).clamp(0, len);
        let mut mm = MinMax::EMPTY;
        for &x in &samples[i0 as usize..i1 as usize] {
            mm.include(x);
        }
        out.push(mm);
    }
}

/// Builds a pyramid from a complete signal.
pub fn build(samples: &[f32]) -> WaveformPyramid {
    let mut b = PyramidBuilder::with_capacity_samples(samples.len() as u64);
    b.push(samples);
    b.finish()
}

/// Incrementally builds a pyramid from a stream of mono samples.
#[derive(Debug, Default)]
pub struct PyramidBuilder {
    level0: Vec<QMinMax>,
    partial: Option<(f32, f32)>,
    partial_len: usize,
    len_samples: u64,
}

impl PyramidBuilder {
    pub fn with_capacity_samples(n: u64) -> Self {
        Self {
            level0: Vec::with_capacity((n / BASE_BUCKET as u64 + 1) as usize),
            ..Self::default()
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.len_samples += samples.len() as u64;
        let mut rest = samples;
        while !rest.is_empty() {
            let take = (BASE_BUCKET - self.partial_len).min(rest.len());
            let (chunk, tail) = rest.split_at(take);
            let (mut lo, mut hi) = self.partial.unwrap_or((f32::INFINITY, f32::NEG_INFINITY));
            for &x in chunk {
                lo = lo.min(x);
                hi = hi.max(x);
            }
            self.partial_len += take;
            if self.partial_len == BASE_BUCKET {
                self.level0.push(QMinMax { min: quantize_down(lo), max: quantize_up(hi) });
                self.partial = None;
                self.partial_len = 0;
            } else {
                self.partial = Some((lo, hi));
            }
            rest = tail;
        }
    }

    pub fn finish(mut self) -> WaveformPyramid {
        if let Some((lo, hi)) = self.partial.take() {
            self.level0.push(QMinMax { min: quantize_down(lo), max: quantize_up(hi) });
        }
        let mut levels = vec![self.level0];
        while levels.last().is_some_and(|l| l.len() > 1) {
            let prev = levels.last().unwrap();
            let next = prev
                .chunks(LEVEL_FACTOR)
                .map(|c| c.iter().copied().fold(QMinMax::EMPTY, QMinMax::merge))
                .collect();
            levels.push(next);
        }
        WaveformPyramid { levels, len_samples: self.len_samples }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, period: f32, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (i as f32 * std::f32::consts::TAU / period).sin()).collect()
    }

    #[test]
    fn chunking_does_not_matter() {
        let s = sine(100_003, 97.0, 0.8);
        let mut a = PyramidBuilder::default();
        a.push(&s);
        let mut b = PyramidBuilder::default();
        for chunk in s.chunks(1000 - 7) {
            b.push(chunk);
        }
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn columns_bound_the_signal() {
        let mut s = sine(1 << 16, 50.0, 0.25);
        s[40_000] = 0.9; // a single spike must survive every level
        let mut b = PyramidBuilder::default();
        b.push(&s);
        let p = b.finish();
        let mut out = Vec::new();
        for spp in [64.0, 100.0, 256.0, 1000.0, 4096.0, 65536.0] {
            let cols = (s.len() as f64 / spp).ceil() as usize;
            p.columns(0.0, spp, cols, &mut out);
            let peak = out.iter().map(|m| m.max).fold(f32::MIN, f32::max);
            assert!(peak >= 0.9, "spike lost at spp={spp}");
            for (c, mm) in out.iter().enumerate() {
                let lo = (c as f64 * spp) as usize;
                let hi = (((c + 1) as f64 * spp) as usize).min(s.len());
                if lo >= hi {
                    continue;
                }
                let tmin = s[lo..hi].iter().copied().fold(f32::MAX, f32::min);
                let tmax = s[lo..hi].iter().copied().fold(f32::MIN, f32::max);
                assert!(mm.min <= tmin + 1e-4 && mm.max >= tmax - 1e-4, "col {c} spp {spp}");
            }
        }
    }

    #[test]
    fn level_selection() {
        let mut b = PyramidBuilder::default();
        b.push(&vec![0.0; 1 << 20]);
        let p = b.finish();
        assert_eq!(p.level_for(10.0), None);
        assert_eq!(p.level_for(64.0), Some(0));
        assert_eq!(p.level_for(255.0), Some(0));
        assert_eq!(p.level_for(256.0), Some(1));
        assert_eq!(p.level_for(1e12), Some(p.levels.len() - 1));
    }

    #[test]
    fn serialization_round_trip() {
        let mut b = PyramidBuilder::default();
        b.push(&sine(12_345, 33.0, 0.5));
        let p = b.finish();
        let mut buf = Vec::new();
        p.write_to(&mut buf).unwrap();
        let q = WaveformPyramid::read_from(&mut buf.as_slice()).unwrap();
        assert_eq!(p, q);
        assert!(WaveformPyramid::read_from(&mut &buf[..10]).is_err());
    }

    #[test]
    fn raw_columns_when_zoomed_in() {
        let s: Vec<f32> = (0..10).map(|i| i as f32 / 10.0).collect();
        let mut out = Vec::new();
        columns_from_raw(&s, 100, 102.0, 0.5, 4, &mut out);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|m| !m.is_empty()));
        assert!((out[0].min - 0.2).abs() < 1e-6);
    }
}
