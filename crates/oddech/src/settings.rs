//! Application preferences that persist between runs (not per project).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhisperModel {
    LargeV3Turbo,
    LargeV3,
}

// Not used by detection any more; kept for cutting retakes later.
#[allow(dead_code)]
impl WhisperModel {
    pub const ALL: [Self; 2] = [Self::LargeV3Turbo, Self::LargeV3];

    pub fn label(self) -> &'static str {
        match self {
            Self::LargeV3Turbo => "large-v3-turbo (szybszy)",
            Self::LargeV3 => "large-v3 (dokładniejszy)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// New video file with the fixed audio tracks (stream copy of video).
    Video,
    /// One WAV file per fixed track.
    WavTracks,
}

impl ExportFormat {
    pub const ALL: [Self; 2] = [Self::Video, Self::WavTracks];

    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "Wideo z poprawionym dźwiękiem",
            Self::WavTracks => "Ścieżki WAV",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WavDepth {
    Float32,
    Int24,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Colormap {
    Magma,
    Viridis,
    Gray,
}

impl Colormap {
    pub const ALL: [Self; 3] = [Self::Magma, Self::Viridis, Self::Gray];

    pub fn label(self) -> &'static str {
        match self {
            Self::Magma => "Magma",
            Self::Viridis => "Viridis",
            Self::Gray => "Skala szarości",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpectrogramSettings {
    pub log_frequency: bool,
    /// Dynamic range shown, in dB below the loudest bin.
    pub db_range: f32,
    pub colormap: Colormap,
    pub fft_size: usize,
}

impl Default for SpectrogramSettings {
    fn default() -> Self {
        Self { log_frequency: true, db_range: 80.0, colormap: Colormap::Magma, fft_size: 1024 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// "Start przed wdechem" in seconds (≥ 0, 0.1 s resolution).
    pub preroll_secs: f64,
    pub play_only_breath: bool,
    pub follow_playhead: bool,
    pub loop_selection: bool,
    /// Show the timecode as HH:MM:SS:FF instead of milliseconds.
    pub timecode_frames: bool,
    pub show_spectrogram: bool,
    pub split_channels: bool,
    pub spectrogram: SpectrogramSettings,
    pub whisper_model: WhisperModel,
    pub export_format: ExportFormat,
    pub wav_depth: WavDepth,
    pub last_dir: Option<PathBuf>,
    /// "Ściszenie": attenuation of breaths in dB, the same for every
    /// recording (`-inf` = mute).
    #[serde(with = "oddech_core::gain::db_serde")]
    pub breath_gain_db: f32,
    /// Minimum model confidence of active detections ("Czułość" = 1 − this).
    pub threshold: f32,
    /// Decoded-audio cache size limit; least recently opened recordings go
    /// first.
    pub cache_limit_gb: u32,
    /// Start timecode of the editing timeline, in whole hours (DaVinci
    /// Resolve starts timelines at 01:00:00:00).
    pub timeline_start_hours: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            preroll_secs: 2.0,
            play_only_breath: false,
            follow_playhead: true,
            loop_selection: false,
            timecode_frames: false,
            show_spectrogram: false,
            split_channels: false,
            spectrogram: SpectrogramSettings::default(),
            whisper_model: WhisperModel::LargeV3Turbo,
            export_format: ExportFormat::Video,
            wav_depth: WavDepth::Float32,
            last_dir: None,
            breath_gain_db: oddech_core::gain::DEFAULT_GAIN_DB,
            threshold: oddech_core::project::DEFAULT_THRESHOLD,
            cache_limit_gb: 20,
            timeline_start_hours: 1,
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "oddech").map(|d| d.config_dir().join("settings.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else { return Self::default() };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Self>(&text)
                .map(Self::sanitized)
                .unwrap_or_else(|e| {
                    log::warn!("ignoring unreadable settings {}: {e}", path.display());
                    Self::default()
                }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, serde_json::to_vec_pretty(self).unwrap_or_default()));
        if let Err(e) = result {
            log::warn!("saving settings failed: {e}");
        }
    }

    fn sanitized(mut self) -> Self {
        self.preroll_secs = round_preroll(self.preroll_secs);
        if !matches!(self.spectrogram.fft_size, 256 | 512 | 1024 | 2048 | 4096) {
            self.spectrogram.fft_size = 1024;
        }
        self.spectrogram.db_range = self.spectrogram.db_range.clamp(30.0, 140.0);
        self
    }
}

/// Clamps to ≥ 0 and rounds to 0.1 s, as the spec requires.
pub fn round_preroll(secs: f64) -> f64 {
    if secs.is_finite() { (secs.max(0.0) * 10.0).round() / 10.0 } else { 2.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preroll_rounding() {
        assert_eq!(round_preroll(-1.0), 0.0);
        assert_eq!(round_preroll(1.26), 1.3);
        assert_eq!(round_preroll(f64::NAN), 2.0);
    }

    #[test]
    fn partial_settings_file_uses_defaults() {
        let s: Settings = serde_json::from_str(r#"{"preroll_secs": 0.5}"#).unwrap();
        assert_eq!(s.preroll_secs, 0.5);
        assert!(s.follow_playhead);
    }
}
