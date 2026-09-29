//! How far each detection is from speech, in loudness terms.
//!
//! The speech level of a track is the loudness only its loudest 2 % of
//! 10 ms frames exceed: the level speech keeps reaching, without single
//! clicks or bumps. For a detection we record, for each distance of 1..=30
//! seconds, how many dB below that level the loudest sound within that
//! distance is. The app's "Dyskryminacja" sliders then decide, without
//! re-analysis, which detections are near speech.

pub const HOP: usize = 160;
const WIN: usize = 400;
const FRAMES_PER_SEC: usize = 100;
pub const PROFILE_SECS: usize = 30;

/// RMS level (dBFS) of 25 ms frames every 10 ms of 16 kHz audio.
pub fn frame_db(audio: &[f32]) -> Vec<f32> {
    let n = audio.len() / HOP + 1;
    let pad = WIN / 2;
    (0..n)
        .map(|f| {
            let start = f * HOP;
            let sum: f64 = (start..start + WIN)
                .filter_map(|i| i.checked_sub(pad).and_then(|i| audio.get(i)))
                .map(|&s| f64::from(s) * f64::from(s))
                .sum();
            (10.0 * (sum / WIN as f64 + 1e-10).log10()) as f32
        })
        .collect()
}

/// Level exceeded by the loudest 2 % of frames.
pub fn speech_level(db: &[f32]) -> f32 {
    if db.is_empty() {
        return 0.0;
    }
    let mut v = db.to_vec();
    let k = ((v.len() - 1) as f64 * 0.98).round() as usize;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// `profile[i]`: dB below `speech_level` of the loudest frame within
/// ±(i + 1) s of `[start, end]` (whole dB, rounded up so that a detection
/// is never hidden by rounding; 0 when at speech level or above).
pub fn silence_profile(db: &[f32], speech_level: f32, start: f64, end: f64) -> Vec<u8> {
    let a = (start * FRAMES_PER_SEC as f64).floor().max(0.0) as usize;
    let b = ((end * FRAMES_PER_SEC as f64).ceil() as usize).min(db.len());
    let max_of = |s: usize, e: usize| db.get(s..e).map_or(f32::NEG_INFINITY, |w| w.iter().copied().fold(f32::NEG_INFINITY, f32::max));
    let mut loudest = max_of(a.min(b), b);
    let mut lo = a;
    let mut hi = b;
    (1..=PROFILE_SECS)
        .map(|_| {
            let new_lo = lo.saturating_sub(FRAMES_PER_SEC);
            let new_hi = (hi + FRAMES_PER_SEC).min(db.len());
            loudest = loudest.max(max_of(new_lo, lo)).max(max_of(hi, new_hi));
            (lo, hi) = (new_lo, new_hi);
            (speech_level - loudest).ceil().clamp(0.0, 255.0) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_sees_speech_at_its_distance() {
        // 60 s of -70 dB with "speech" (-20 dB) from 40 s on.
        let mut db = vec![-70.0f32; 60 * 100];
        db[4000..].fill(-20.0);
        let p = silence_profile(&db, -20.0, 30.0, 30.5);
        assert_eq!(p.len(), PROFILE_SECS);
        assert_eq!(p[0], 50, "nothing but silence within 1 s");
        assert_eq!(p[8], 50, "9 s reaches 39.5 s");
        assert_eq!(p[9], 0, "10 s reaches 40.5 s");
    }

    #[test]
    fn speech_level_ignores_rare_peaks() {
        let mut db = vec![-60.0f32; 1000];
        db[..100].fill(-25.0); // 10 % speech
        db[500] = 0.0; // a single click
        assert_eq!(speech_level(&db), -25.0);
    }

    #[test]
    fn frame_level_of_full_scale_square_is_zero_db() {
        let audio: Vec<f32> = (0..16000).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let db = frame_db(&audio);
        assert!(db[50].abs() < 1e-3);
    }
}
