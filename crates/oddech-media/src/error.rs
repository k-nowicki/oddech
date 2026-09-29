use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("nie można otworzyć pliku {path}: {source}")]
    Open { path: PathBuf, source: ffmpeg_next::Error },
    #[error("plik nie zawiera ścieżek audio")]
    NoAudio,
    #[error("błąd FFmpeg: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),
    #[error("błąd wejścia/wyjścia: {0}")]
    Io(#[from] std::io::Error),
    #[error("nieobsługiwany format próbek: {0}")]
    UnsupportedSampleFormat(String),
    #[error("przerwano")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}
