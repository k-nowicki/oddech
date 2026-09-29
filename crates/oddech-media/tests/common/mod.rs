//! Shared helpers for integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// 3 s video + three audio tracks with different layouts and rates:
/// 0: 440 Hz sine, mono, 48 kHz, amplitude 0.5 (lavfi sine is 1/8, ×4)
/// 1: 1 kHz sine, stereo, 44.1 kHz (right channel silent)
/// 2: white noise, stereo, 48 kHz
pub fn make_synthetic(dir: &Path, ext: &str) -> PathBuf {
    let out = dir.join(format!("synthetic.{ext}"));
    let codec = if ext == "mkv" { "flac" } else { "aac" };
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=160x120:rate=25:duration=3"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3"])
        .args(["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100:duration=3"])
        .args(["-f", "lavfi", "-i", "anoisesrc=color=white:sample_rate=48000:amplitude=0.3:duration=3"])
        .args(["-filter_complex", "[1:a]volume=4[a0];[2:a]pan=stereo|c0=c0|c1=0*c0[a1];[3:a]pan=stereo|c0=c0|c1=c0[a2]"])
        .args(["-map", "0:v", "-map", "[a0]", "-map", "[a1]", "-map", "[a2]"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", codec])
        .args(["-metadata:s:a:0", "title=Sinus", "-metadata:s:a:1", "language=pol"])
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "ffmpeg failed to create the synthetic file");
    out
}

