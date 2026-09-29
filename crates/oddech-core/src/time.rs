//! Time helpers: conversions between seconds and sample indices, and
//! timecode formatting for the ruler and transport display.

/// Converts a time in seconds to the nearest sample index at `rate`.
///
/// All code that maps region boundaries to samples must go through this
/// function so that playback and export agree sample-for-sample.
#[inline]
pub fn secs_to_sample(secs: f64, rate: u32) -> i64 {
    (secs * f64::from(rate)).round() as i64
}

#[inline]
pub fn sample_to_secs(sample: i64, rate: u32) -> f64 {
    sample as f64 / f64::from(rate)
}

/// Formats as `HH:MM:SS.mmm` (hours are always shown).
pub fn format_hms_millis(secs: f64) -> String {
    let secs = secs.max(0.0);
    let total_ms = (secs * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60_000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// Formats as `HH:MM:SS:FF` using the given frame rate.
pub fn format_hms_frames(secs: f64, fps: f64) -> String {
    let secs = secs.max(0.0);
    let whole = secs.floor();
    let frame = ((secs - whole) * fps).floor() as u64;
    let whole = whole as u64;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        whole / 3600,
        (whole / 60) % 60,
        whole % 60,
        frame
    )
}

/// Compact label for ruler ticks. The precision adapts to the tick spacing
/// so labels stay short: `12:04`, `12:04.5`, `12:04.250`, `1:02:03`.
pub fn format_ruler_label(secs: f64, tick_step: f64) -> String {
    let secs = secs.max(0.0);
    let decimals = if tick_step >= 1.0 {
        0
    } else if tick_step >= 0.1 {
        1
    } else if tick_step >= 0.01 {
        2
    } else {
        3
    };
    let scale = 10f64.powi(decimals);
    let total = (secs * scale).round() / scale;
    let whole = total.floor() as u64;
    let frac = total - whole as f64;
    let h = whole / 3600;
    let m = (whole / 60) % 60;
    let s = whole % 60;
    let mut out = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    };
    if decimals > 0 {
        let f = (frac * scale).round() as u64;
        out.push_str(&format!(".{f:0width$}", width = decimals as usize));
    }
    out
}

/// Chooses a "nice" ruler tick step (1-2-5 sequence plus clock-friendly
/// multiples of seconds/minutes) that is at least `min_step` seconds.
pub fn nice_tick_step(min_step: f64) -> f64 {
    const STEPS: &[f64] = &[
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0,
        60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
    ];
    STEPS
        .iter()
        .copied()
        .find(|&s| s >= min_step)
        .unwrap_or_else(|| (min_step / 3600.0).ceil() * 3600.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_millis() {
        assert_eq!(format_hms_millis(0.0), "00:00:00.000");
        assert_eq!(format_hms_millis(734.75), "00:12:14.750");
        assert_eq!(format_hms_millis(3661.0005), "01:01:01.001");
    }

    #[test]
    fn hms_frames() {
        assert_eq!(format_hms_frames(1.5, 30.0), "00:00:01:15");
    }

    #[test]
    fn ruler_labels() {
        assert_eq!(format_ruler_label(724.0, 2.0), "12:04");
        assert_eq!(format_ruler_label(724.5, 0.5), "12:04.5");
        assert_eq!(format_ruler_label(3723.0, 60.0), "1:02:03");
        assert_eq!(format_ruler_label(1.25, 0.05), "0:01.25");
    }

    #[test]
    fn sample_conversion_round_trips() {
        for &rate in &[44_100u32, 48_000] {
            for s in [0i64, 1, 44_099, 1_000_000] {
                assert_eq!(secs_to_sample(sample_to_secs(s, rate), rate), s);
            }
        }
    }

    #[test]
    fn nice_steps() {
        assert_eq!(nice_tick_step(0.0007), 0.001);
        assert_eq!(nice_tick_step(1.4), 2.0);
        assert_eq!(nice_tick_step(40.0), 60.0);
        assert_eq!(nice_tick_step(5000.0), 7200.0);
    }
}
