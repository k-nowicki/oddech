//! Audio track metadata.

use serde::{Deserialize, Serialize};

/// Stable identifier of an audio track: the stream index inside the
/// container. Stream indices are stable across re-opens of the same file,
/// which makes them suitable as keys in the project file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrackId(pub u32);

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Everything we know about an audio stream before decoding it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    pub id: TrackId,
    /// Position among audio streams only (0 = first audio stream).
    pub audio_ordinal: usize,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec: String,
    pub channels: u16,
    pub sample_rate: u32,
    /// Duration in seconds as reported by the container (may be approximate).
    pub duration_secs: f64,
}

impl TrackInfo {
    /// Human-facing name: title tag, then language tag, then a generic label.
    pub fn display_name(&self) -> String {
        match (&self.title, &self.language) {
            (Some(t), _) if !t.trim().is_empty() => t.trim().to_owned(),
            (_, Some(l)) if !l.trim().is_empty() && l != "und" => {
                format!("Ścieżka {} ({})", self.audio_ordinal + 1, l.trim())
            }
            _ => format!("Ścieżka {}", self.audio_ordinal + 1),
        }
    }

    pub fn channel_layout_label(&self) -> &'static str {
        match self.channels {
            1 => "mono",
            2 => "stereo",
            6 => "5.1",
            8 => "7.1",
            _ => "wielokanałowa",
        }
    }
}
