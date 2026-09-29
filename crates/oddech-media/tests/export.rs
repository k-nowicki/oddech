//! Export tests: WAV output matches the playback render path exactly, and
//! remuxing replaces only the fixed tracks.

mod common;

use std::sync::Arc;

use common::{ffmpeg_available, make_synthetic};
use oddech_core::gain::{DuckParams, GainEnvelope};
use oddech_core::render::render_source_block;
use oddech_core::{Region, RegionId, RegionKind, RegionSource, TrackId};
use oddech_media::export::{ExportTrack, WavSampleFormat, export_wav, remux, unique_output_path};
use oddech_media::{ImportProgress, import_media, probe};

fn breath(track: TrackId, start: f64, end: f64) -> Region {
    Region {
        id: RegionId(1),
        track,
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

#[test]
fn wav_export_matches_playback_render_sample_for_sample() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = make_synthetic(dir.path(), "mkv");
    let progress = Arc::new(ImportProgress::default());
    let media = import_media(&path, &dir.path().join("cache"), &progress).unwrap();
    let tr = &media.tracks[2]; // stereo noise, 48 kHz
    let env = GainEnvelope::build(
        &[breath(tr.info.id, 1.0, 1.4), breath(tr.info.id, 2.0, 2.05)],
        &DuckParams { gain_db: -18.0, fade_ms: 15.0 },
        &oddech_core::Gate { threshold: 0.5, discrimination: Default::default() },
        tr.info.sample_rate,
    );
    let out = unique_output_path(&path, "test", "wav");
    export_wav(&ExportTrack { id: tr.info.id, store: &tr.store, envelope: &env }, &out, WavSampleFormat::Float32, &progress)
        .unwrap();

    let mut reader = hound::WavReader::open(&out).unwrap();
    assert_eq!(reader.spec().channels, 2);
    assert_eq!(reader.spec().sample_rate, 48_000);
    let wav: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(wav.len(), tr.store.len() * 2);

    // What the playback mixer renders for the same parameters.
    let mut expected = vec![vec![0.0f32; tr.store.len()]; 2];
    let mut scratch = Vec::new();
    for (ch, buf) in expected.iter_mut().enumerate() {
        render_source_block(&tr.store, ch, Some(&env), 0, buf, &mut scratch);
    }
    for (i, frame) in wav.chunks_exact(2).enumerate() {
        assert_eq!(frame[0].to_bits(), expected[0][i].to_bits(), "L sample {i}");
        assert_eq!(frame[1].to_bits(), expected[1][i].to_bits(), "R sample {i}");
    }
    // Attenuation actually happened inside the region, not outside.
    let rms = |a: usize, b: usize| (wav[a * 2..b * 2].iter().map(|x| x * x).sum::<f32>() / (2 * (b - a)) as f32).sqrt();
    let inside = rms(50_000, 66_000);
    let outside = rms(10_000, 26_000);
    assert!((20.0 * (inside / outside).log10() + 18.0).abs() < 0.5, "{inside} vs {outside}");

    // Source buffers are untouched by export.
    let again = import_media(&path, &dir.path().join("cache"), &progress).unwrap();
    assert_eq!(again.tracks[2].store.channel(0), tr.store.channel(0));

    // A second export never overwrites the first.
    assert_ne!(unique_output_path(&path, "test", "wav"), out);
}

#[test]
fn remux_replaces_only_fixed_tracks() {
    if !ffmpeg_available() {
        return;
    }
    for ext in ["mkv", "mp4"] {
        let dir = tempfile::tempdir().unwrap();
        let path = make_synthetic(dir.path(), ext);
        let progress = Arc::new(ImportProgress::default());
        let media = import_media(&path, &dir.path().join("cache"), &progress).unwrap();
        let fixed = &media.tracks[0]; // 440 Hz sine, mono
        let env = GainEnvelope::build(
            &[breath(fixed.info.id, 1.0, 2.0)],
            &DuckParams { gain_db: -20.0, fade_ms: 10.0 },
            &oddech_core::Gate { threshold: 0.5, discrimination: Default::default() },
            fixed.info.sample_rate,
        );
        let out = unique_output_path(&path, "oddech", ext);
        remux(&path, &out, &[ExportTrack { id: fixed.info.id, store: &fixed.store, envelope: &env }], &progress)
            .unwrap();

        let before = probe(&path).unwrap();
        let after = probe(&out).unwrap();
        assert_eq!(after.tracks.len(), before.tracks.len(), "{ext}");
        assert_eq!(after.video_fps, before.video_fps);
        for (a, b) in after.tracks.iter().zip(&before.tracks) {
            assert_eq!((a.id, a.channels, a.sample_rate, &a.codec), (b.id, b.channels, b.sample_rate, &b.codec));
            assert_eq!(a.title, b.title, "{ext}: metadata kept");
        }

        let result = import_media(&out, &dir.path().join("cache"), &progress).unwrap();
        let new = result.tracks[0].store.channel(0);
        let old = fixed.store.channel(0);
        let rate = fixed.info.sample_rate as usize;
        let peak = |x: &[f32]| x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        // Attenuated in the region (−20 dB → 0.05), untouched elsewhere.
        assert!(peak(&new[rate * 12 / 10..rate * 18 / 10]) < 0.06, "{ext}");
        assert!((peak(&new[rate * 25 / 10..rate * 28 / 10]) - 0.5).abs() < 0.03, "{ext}");
        if ext == "mkv" {
            // FLAC round trip: samples outside the region are bit-identical.
            assert_eq!(&new[..rate * 9 / 10], &old[..rate * 9 / 10]);
            assert_eq!(new.len(), old.len());
            // Copied tracks are identical too.
            assert_eq!(result.tracks[2].store.channel(1), media.tracks[2].store.channel(1));
        } else {
            let diff = (new.len() as i64 - old.len() as i64).abs();
            assert!(diff < 2048, "length changed by {diff}");
        }
    }
}
