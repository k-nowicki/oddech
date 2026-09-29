//! Parallel decoding of audio streams to planar `f32`.
//!
//! The container is demuxed once on the calling thread; packets of each
//! selected stream are handed to a dedicated decoder thread through a
//! bounded channel. Decoding FLAC/AAC/Opus is CPU-bound, so this scales
//! almost linearly with the number of tracks.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::bounded;
use ffmpeg_next as ff;
use ff::format::sample::Type as SampleLayout;
use ff::format::Sample;
use oddech_core::TrackInfo;

use crate::MediaError;

/// Receives decoded audio for one track, in order, as planar channels of
/// equal length.
pub trait PlanarSink: Send {
    fn push(&mut self, planes: &[&[f32]]) -> std::io::Result<()>;
    /// Called once after the last block when decoding succeeded.
    fn finish(self: Box<Self>) -> std::io::Result<()>;
}

/// Leading silence longer than this is assumed to be a broken timestamp
/// rather than a genuinely late-starting stream.
const MAX_LEADING_PAD_SECS: f64 = 600.0;

/// Decodes the given tracks of `path`, feeding each track's sink. Returns the
/// number of samples per channel delivered to each sink.
///
/// `progress` receives the demux position in seconds. Setting `cancel`
/// aborts early with [`MediaError::Cancelled`].
pub fn decode_tracks(
    path: &Path,
    tracks: &[TrackInfo],
    sinks: Vec<Box<dyn PlanarSink>>,
    progress: &(dyn Fn(f64) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<u64>, MediaError> {
    crate::init();
    assert_eq!(tracks.len(), sinks.len());
    let mut ictx = ff::format::input(path)
        .map_err(|source| MediaError::Open { path: path.to_owned(), source })?;
    // SAFETY: reading a plain field of a valid, opened format context.
    let start_time = unsafe { (*ictx.as_ptr()).start_time };
    let global_start = if start_time == ff::ffi::AV_NOPTS_VALUE {
        0.0
    } else {
        start_time as f64 / f64::from(ff::ffi::AV_TIME_BASE)
    };

    // Decoder setup happens here so that errors surface before threads spawn.
    let mut jobs = Vec::with_capacity(tracks.len());
    for (track, sink) in tracks.iter().zip(sinks) {
        let stream = ictx
            .stream(track.id.0 as usize)
            .ok_or_else(|| MediaError::Other(format!("brak strumienia {}", track.id)))?;
        let mut ctx = ff::codec::context::Context::from_parameters(stream.parameters())?;
        // Let FFmpeg use its own frame threading where the codec supports it.
        ctx.set_threading(ff::codec::threading::Config::default());
        let mut decoder = ctx.decoder().audio()?;
        decoder.set_packet_time_base(stream.time_base());
        jobs.push((track.clone(), decoder, stream.time_base(), sink));
    }
    let index_of: Vec<Option<usize>> = {
        let mut map = vec![None; ictx.nb_streams() as usize];
        for (i, t) in tracks.iter().enumerate() {
            map[t.id.0 as usize] = Some(i);
        }
        map
    };

    std::thread::scope(|scope| {
        let mut senders = Vec::with_capacity(jobs.len());
        let mut handles = Vec::with_capacity(jobs.len());
        for (track, decoder, tb, sink) in jobs {
            let (tx, rx) = bounded::<ff::Packet>(256);
            senders.push(tx);
            handles.push(scope.spawn(move || {
                let mut w = TrackDecoder::new(track, decoder, tb, global_start, sink);
                for packet in rx {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(MediaError::Cancelled);
                    }
                    w.feed(Some(&packet))?;
                }
                w.feed(None)?;
                let delivered = w.delivered;
                w.sink.finish()?;
                Ok(delivered)
            }));
        }

        let mut last_report = -1.0;
        let mut demux_result = Ok(());
        for (stream, packet) in ictx.packets() {
            if cancel.load(Ordering::Relaxed) {
                demux_result = Err(MediaError::Cancelled);
                break;
            }
            let Some(Some(i)) = index_of.get(stream.index()) else { continue };
            if let Some(pts) = packet.pts().or(packet.dts()) {
                let t = pts as f64 * f64::from(stream.time_base()) - global_start;
                if t - last_report > 0.25 {
                    last_report = t;
                    progress(t);
                }
            }
            if senders[*i].send(packet).is_err() {
                // The worker bailed out; its error is reported below.
                break;
            }
        }
        drop(senders);

        let mut delivered = Vec::with_capacity(handles.len());
        for h in handles {
            match h.join() {
                Ok(Ok(n)) => delivered.push(n),
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(MediaError::Other("wątek dekodera zakończył się błędem".into())),
            }
        }
        demux_result.map(|()| delivered)
    })
}

struct TrackDecoder {
    track: TrackInfo,
    decoder: ff::decoder::Audio,
    time_base: ff::Rational,
    global_start: f64,
    sink: Box<dyn PlanarSink>,
    frame: ff::frame::Audio,
    planes: Vec<Vec<f32>>,
    /// Samples per channel handed to the sink so far.
    delivered: u64,
    /// Samples still to be dropped (negative start offset / priming).
    to_skip: u64,
    started: bool,
}

impl TrackDecoder {
    fn new(
        track: TrackInfo,
        decoder: ff::decoder::Audio,
        time_base: ff::Rational,
        global_start: f64,
        sink: Box<dyn PlanarSink>,
    ) -> Self {
        let channels = usize::from(track.channels.max(1));
        Self {
            track,
            decoder,
            time_base,
            global_start,
            sink,
            frame: ff::frame::Audio::empty(),
            planes: vec![Vec::new(); channels],
            delivered: 0,
            to_skip: 0,
            started: false,
        }
    }

    fn feed(&mut self, packet: Option<&ff::Packet>) -> Result<(), MediaError> {
        let sent = match packet {
            Some(p) => self.decoder.send_packet(p),
            None => self.decoder.send_eof(),
        };
        match sent {
            Ok(()) => {}
            // Corrupt packets happen in real recordings; skip rather than fail.
            Err(ff::Error::InvalidData) => {
                log::warn!("track {}: skipping invalid packet", self.track.id);
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        loop {
            match self.decoder.receive_frame(&mut self.frame) {
                Ok(()) => self.handle_frame()?,
                Err(ff::Error::Eof) => return Ok(()),
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => return Ok(()),
                Err(ff::Error::InvalidData) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn handle_frame(&mut self) -> Result<(), MediaError> {
        let n = self.frame.samples();
        if n == 0 {
            return Ok(());
        }
        let rate = f64::from(self.track.sample_rate);
        if !self.started {
            self.started = true;
            if let Some(pts) = self.frame.pts() {
                let offset = pts as f64 * f64::from(self.time_base) - self.global_start;
                let offset_samples = (offset * rate).round() as i64;
                if offset > 0.0 && offset < MAX_LEADING_PAD_SECS {
                    self.pad_silence(offset_samples as usize)?;
                } else if offset_samples < 0 {
                    self.to_skip = offset_samples.unsigned_abs();
                }
            }
        }

        convert_to_planar(&self.frame, &mut self.planes)?;
        let skip = (self.to_skip as usize).min(n);
        self.to_skip -= skip as u64;
        if skip == n {
            return Ok(());
        }
        let views: Vec<&[f32]> = self.planes.iter().map(|p| &p[skip..n]).collect();
        self.sink.push(&views)?;
        self.delivered += (n - skip) as u64;
        Ok(())
    }

    fn pad_silence(&mut self, mut count: usize) -> Result<(), MediaError> {
        const BLOCK: usize = 16_384;
        let zeros = vec![0.0f32; BLOCK.min(count)];
        while count > 0 {
            let k = count.min(BLOCK);
            let views: Vec<&[f32]> = self.planes.iter().map(|_| &zeros[..k]).collect();
            self.sink.push(&views)?;
            self.delivered += k as u64;
            count -= k;
        }
        Ok(())
    }
}

/// Converts any FFmpeg sample format into planar `f32` in `planes`
/// (resized to the frame length). If the frame's channel count differs from
/// the expected count, extra channels are dropped and missing ones zeroed.
pub fn convert_to_planar(frame: &ff::frame::Audio, planes: &mut [Vec<f32>]) -> Result<(), MediaError> {
    let n = frame.samples();
    let src_ch = usize::from(frame.channels());
    for p in planes.iter_mut() {
        p.clear();
        p.resize(n, 0.0);
    }
    let fmt = frame.format();
    macro_rules! convert {
        ($ty:ty, $to_f32:expr) => {{
            let to_f32 = $to_f32;
            match fmt {
                Sample::U8(SampleLayout::Planar)
                | Sample::I16(SampleLayout::Planar)
                | Sample::I32(SampleLayout::Planar)
                | Sample::F32(SampleLayout::Planar)
                | Sample::F64(SampleLayout::Planar) => {
                    for (ch, dst) in planes.iter_mut().enumerate().take(src_ch) {
                        let src: &[$ty] = frame.plane(ch);
                        for (d, s) in dst.iter_mut().zip(&src[..n]) {
                            *d = to_f32(*s);
                        }
                    }
                }
                _ => {
                    let src: &[$ty] = frame.plane(0);
                    for (i, chunk) in src[..n * src_ch].chunks_exact(src_ch).enumerate() {
                        for (dst, s) in planes.iter_mut().zip(chunk) {
                            dst[i] = to_f32(*s);
                        }
                    }
                }
            }
        }};
    }
    match fmt {
        Sample::U8(_) => convert!(u8, |s: u8| (f32::from(s) - 128.0) / 128.0),
        Sample::I16(_) => convert!(i16, |s: i16| f32::from(s) / 32768.0),
        Sample::I32(_) => convert!(i32, |s: i32| (s as f64 / 2_147_483_648.0) as f32),
        Sample::F32(_) => convert!(f32, |s: f32| s),
        Sample::F64(_) => convert!(f64, |s: f64| s as f32),
        // No real-world audio decoder emits 64-bit integer samples.
        Sample::I64(_) | Sample::None => {
            return Err(MediaError::UnsupportedSampleFormat(format!("{fmt:?}")));
        }
    }
    Ok(())
}
