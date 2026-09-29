//! Stateless band-limited interpolation for playback-rate conversion.
//!
//! Because source audio is random-access, we do not need a streaming
//! resampler with internal state: each output sample is computed directly
//! from source samples around its fractional source position. Seeking,
//! looping and scrubbing therefore need no flushing or priming.
//!
//! The kernel is a Kaiser-windowed sinc, tabulated for [`PHASES`] sub-sample
//! offsets with linear interpolation between neighbouring phases. When
//! downsampling, the cutoff is lowered to the target Nyquist to avoid
//! aliasing.

/// Taps per output sample (must be even).
pub const TAPS: usize = 32;
const HALF: usize = TAPS / 2;
const PHASES: usize = 512;
const KAISER_BETA: f64 = 8.0;

#[derive(Debug, Clone)]
pub struct SincInterpolator {
    /// Source samples advanced per output sample (`src_rate / dst_rate`).
    step: f64,
    /// `(PHASES + 1) × TAPS` coefficients; the extra row simplifies
    /// interpolation between phases.
    table: Vec<f32>,
}

impl SincInterpolator {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        let step = f64::from(src_rate) / f64::from(dst_rate);
        // Cutoff relative to source Nyquist, slightly below to leave room
        // for the transition band.
        let cutoff = (1.0 / step).min(1.0) * 0.95;
        let mut table = vec![0.0f32; (PHASES + 1) * TAPS];
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row = &mut table[p * TAPS..(p + 1) * TAPS];
            let mut sum = 0.0;
            for (j, c) in row.iter_mut().enumerate() {
                // Tap j sits at source offset (j - HALF + 1) relative to
                // floor(position); distance from the exact position:
                let x = (j as f64 - HALF as f64 + 1.0) - frac;
                let v = cutoff * sinc(cutoff * x) * kaiser(x / HALF as f64);
                *c = v as f32;
                sum += v;
            }
            // Normalize for unity DC gain.
            for c in row.iter_mut() {
                *c = (f64::from(*c) / sum) as f32;
            }
        }
        Self { step, table }
    }

    pub fn step(&self) -> f64 {
        self.step
    }

    /// Source samples needed before `floor(pos)` and after it.
    pub const fn margins() -> (usize, usize) {
        (HALF - 1, HALF)
    }

    /// Computes `out.len()` samples starting at fractional source position
    /// `pos`. `src[0]` corresponds to absolute source sample `src_start`;
    /// `src` must cover `[floor(pos) - HALF + 1, floor(last) + HALF]`
    /// (use [`Self::source_window`]).
    pub fn process(&self, src: &[f32], src_start: i64, pos: f64, out: &mut [f32]) {
        for (k, o) in out.iter_mut().enumerate() {
            let t = pos + k as f64 * self.step;
            let base = t.floor();
            let frac = t - base;
            let first = base as i64 - (HALF as i64 - 1) - src_start;
            debug_assert!(first >= 0 && first as usize + TAPS <= src.len());
            let window = &src[first as usize..first as usize + TAPS];

            let fp = frac * PHASES as f64;
            let p = (fp as usize).min(PHASES - 1);
            let w = (fp - p as f64) as f32;
            let a = &self.table[p * TAPS..(p + 1) * TAPS];
            let b = &self.table[(p + 1) * TAPS..(p + 2) * TAPS];
            let mut acc = 0.0f32;
            for j in 0..TAPS {
                let c = a[j] + (b[j] - a[j]) * w;
                acc += c * window[j];
            }
            *o = acc;
        }
    }

    /// The absolute source range `[start, end)` needed to produce `n`
    /// output samples starting at `pos`.
    pub fn source_window(&self, pos: f64, n: usize) -> (i64, i64) {
        let last = pos + n.saturating_sub(1) as f64 * self.step;
        (pos.floor() as i64 - (HALF as i64 - 1), last.floor() as i64 + HALF as i64 + 1)
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Kaiser window on `[-1, 1]`.
fn kaiser(x: f64) -> f64 {
    if x.abs() > 1.0 {
        return 0.0;
    }
    bessel_i0(KAISER_BETA * (1.0 - x * x).sqrt()) / bessel_i0(KAISER_BETA)
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(interp: &SincInterpolator, src: &[f32], pos: f64, n: usize) -> Vec<f32> {
        let (s, e) = interp.source_window(pos, n);
        let window: Vec<f32> = (s..e)
            .map(|i| if i >= 0 && (i as usize) < src.len() { src[i as usize] } else { 0.0 })
            .collect();
        let mut out = vec![0.0; n];
        interp.process(&window, s, pos, &mut out);
        out
    }

    fn sine(rate: f64, freq: f64, n: usize, offset: f64) -> Vec<f32> {
        (0..n)
            .map(|i| ((i as f64 + offset) / rate * freq * std::f64::consts::TAU).sin() as f32)
            .collect()
    }

    #[test]
    fn identity_ratio_reproduces_input() {
        let interp = SincInterpolator::new(48_000, 48_000);
        let src = sine(48_000.0, 1000.0, 4000, 0.0);
        let out = run(&interp, &src, 100.0, 1000);
        for (a, b) in out.iter().zip(&src[100..1100]) {
            assert!((a - b).abs() < 2e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn upsampling_44k1_to_48k_preserves_a_tone() {
        let interp = SincInterpolator::new(44_100, 48_000);
        let src = sine(44_100.0, 1000.0, 44_100, 0.0);
        let out = run(&interp, &src, 1000.0, 2000);
        // Expected: the same tone sampled at 48 kHz, starting at source
        // position 1000 (i.e. 1000/44100 s).
        let t0 = 1000.0 / 44_100.0;
        for (k, o) in out.iter().enumerate() {
            let t = t0 + k as f64 / 48_000.0;
            let expected = (t * 1000.0 * std::f64::consts::TAU).sin() as f32;
            assert!((o - expected).abs() < 5e-3, "k={k}: {o} vs {expected}");
        }
    }

    #[test]
    fn downsampling_rejects_content_above_new_nyquist() {
        // 30 kHz tone at 96 kHz → 44.1 kHz must be strongly attenuated.
        let interp = SincInterpolator::new(96_000, 44_100);
        let src = sine(96_000.0, 30_000.0, 96_000, 0.0);
        let out = run(&interp, &src, 2000.0, 4000);
        let rms = (out.iter().map(|x| x * x).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms < 0.02, "alias rms {rms}");
    }

    #[test]
    fn block_splitting_is_seamless() {
        let interp = SincInterpolator::new(44_100, 48_000);
        let src = sine(44_100.0, 440.0, 20_000, 0.0);
        let whole = run(&interp, &src, 500.0, 3000);
        let mut parts = run(&interp, &src, 500.0, 1234);
        parts.extend(run(&interp, &src, 500.0 + 1234.0 * interp.step(), 3000 - 1234));
        for (a, b) in whole.iter().zip(&parts) {
            assert!((a - b).abs() < 1e-4);
        }
    }
}
