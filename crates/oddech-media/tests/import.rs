//! End-to-end import of a synthetic multi-track file generated with the
//! `ffmpeg` CLI (tests are skipped when it is not installed).

mod common;

use std::sync::Arc;

use common::{ffmpeg_available, make_synthetic};

use oddech_media::{ImportProgress, import_media, probe};

#[test]
fn probe_reports_all_audio_streams() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = make_synthetic(dir.path(), "mkv");
    let info = probe(&path).unwrap();
    assert_eq!(info.tracks.len(), 3);
    assert_eq!(info.video_fps, Some(25.0));
    let t = &info.tracks;
    assert_eq!((t[0].channels, t[0].sample_rate), (1, 48_000));
    assert_eq!((t[1].channels, t[1].sample_rate), (2, 44_100));
    assert_eq!((t[2].channels, t[2].sample_rate), (2, 48_000));
    assert_eq!(t[0].display_name(), "Sinus");
    assert_eq!(t[1].display_name(), "Ścieżka 2 (pol)");
    assert_eq!(t[0].codec, "flac");
}

#[test]
fn import_decodes_and_caches() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cache_root = dir.path().join("cache");
    for ext in ["mkv", "mp4"] {
        let path = make_synthetic(dir.path(), ext);
        let progress = Arc::new(ImportProgress::default());
        let media = import_media(&path, &cache_root, &progress).unwrap();
        assert!(!media.from_cache);
        assert_eq!(progress.fraction(), 1.0);

        for tr in &media.tracks {
            let expected = 3.0 * f64::from(tr.info.sample_rate);
            let len = tr.store.len() as f64;
            // AAC adds/drops at most a frame or two of padding.
            assert!((len - expected).abs() < 4096.0, "{ext} track {} len {len}", tr.info.id);
        }

        // Track 0: 440 Hz at amplitude 0.5 (lossless in MKV).
        let s = media.tracks[0].store.channel(0);
        let peak = s[4800..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.03, "{ext}: peak {peak}");

        // Track 1: signal on the left, silence on the right.
        let st = &media.tracks[1].store;
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        assert!(rms(&st.channel(0)[4410..40_000]) > 0.05);
        assert!(rms(&st.channel(1)[4410..40_000]) < 1e-3);

        // Pyramid covers the whole signal.
        assert_eq!(media.tracks[0].pyramid.len_samples(), media.tracks[0].store.len() as u64);

        // Second import is served from cache with identical data.
        let again = import_media(&path, &cache_root, &progress).unwrap();
        assert!(again.from_cache);
        assert_eq!(again.tracks[2].store.channel(1), media.tracks[2].store.channel(1));
        assert_eq!(again.tracks[0].pyramid, media.tracks[0].pyramid);
    }
}
