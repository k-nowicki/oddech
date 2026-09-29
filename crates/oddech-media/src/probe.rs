//! Container inspection without decoding.

use std::path::{Path, PathBuf};

use ffmpeg_next as ff;
use oddech_core::{TrackId, TrackInfo};

use crate::MediaError;

#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub container: String,
    pub duration_secs: f64,
    /// Frame rate of the first video stream, if any.
    pub video_fps: Option<f64>,
    /// Frame size of the first video stream, if any.
    pub video_size: Option<(u32, u32)>,
    pub tracks: Vec<TrackInfo>,
}

pub fn probe(path: &Path) -> Result<MediaInfo, MediaError> {
    crate::init();
    let ictx = ff::format::input(path)
        .map_err(|source| MediaError::Open { path: path.to_owned(), source })?;

    let container_secs = if ictx.duration() > 0 {
        ictx.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE)
    } else {
        0.0
    };

    let mut video_fps = None;
    let mut video_size = None;
    let mut tracks = Vec::new();
    for stream in ictx.streams() {
        let params = stream.parameters();
        match params.medium() {
            ff::media::Type::Video if video_size.is_none() => {
                let r = stream.avg_frame_rate();
                if r.numerator() > 0 && r.denominator() > 0 {
                    video_fps = Some(f64::from(r));
                }
                // SAFETY: plain field reads from valid codec parameters.
                let (w, h) = unsafe { ((*params.as_ptr()).width, (*params.as_ptr()).height) };
                if w > 0 && h > 0 {
                    video_size = Some((w as u32, h as u32));
                }
            }
            ff::media::Type::Audio => {
                let ctx = ff::codec::context::Context::from_parameters(params)?;
                let dec = ctx.decoder().audio()?;
                let meta = stream.metadata();
                let tb = stream.time_base();
                let duration_secs = if stream.duration() > 0 {
                    stream.duration() as f64 * f64::from(tb)
                } else {
                    container_secs
                };
                tracks.push(TrackInfo {
                    id: TrackId(stream.index() as u32),
                    audio_ordinal: tracks.len(),
                    title: meta.get("title").or_else(|| meta.get("handler_name")).map(str::to_owned),
                    language: meta.get("language").map(str::to_owned),
                    codec: dec.id().name().to_owned(),
                    channels: dec.channels(),
                    sample_rate: dec.rate(),
                    duration_secs,
                });
            }
            _ => {}
        }
    }
    if tracks.is_empty() {
        return Err(MediaError::NoAudio);
    }
    // Generic MP4 handler names carry no information; drop them.
    for t in &mut tracks {
        if t.title.as_deref().is_some_and(|n| n == "SoundHandler" || n == "Sound Media Handler") {
            t.title = None;
        }
    }
    Ok(MediaInfo {
        path: path.to_owned(),
        container: ictx.format().name().to_owned(),
        duration_secs: container_secs.max(tracks.iter().map(|t| t.duration_secs).fold(0.0, f64::max)),
        video_fps,
        video_size,
        tracks,
    })
}
