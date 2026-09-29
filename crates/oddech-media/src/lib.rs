//! Media I/O on top of FFmpeg: probing containers, decoding audio streams
//! into the sample cache, and (in [`export`]) writing results back out.

pub mod decode;
pub mod error;
pub mod export;
pub mod import;
pub mod probe;

pub use error::MediaError;
pub use import::{ImportProgress, ImportedMedia, ImportedTrack, import_media};
pub use probe::{MediaInfo, probe};

use std::sync::Once;

/// Initializes FFmpeg once per process and quiets its logging.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if let Err(e) = ffmpeg_next::init() {
            log::error!("FFmpeg init failed: {e}");
        }
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
    });
}
