//! Writing results: per-track WAV files and a remuxed video with the fixed
//! audio tracks re-encoded (everything else is stream-copied).
//!
//! Processed audio always comes from
//! [`oddech_core::render::render_source_block`], the same function the
//! playback path uses, so exports match what was heard.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use ffmpeg_next as ff;
use ff::ffi;
use ff::format::Sample;
use ff::format::sample::Type as SampleLayout;
use oddech_core::TrackId;
use oddech_core::gain::GainEnvelope;
use oddech_core::render::render_source_block;
use oddech_core::store::TrackStore;

use crate::{ImportProgress as Progress, MediaError};

/// One processed track to export.
pub struct ExportTrack<'a> {
    pub id: TrackId,
    pub store: &'a TrackStore,
    pub envelope: &'a GainEnvelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavSampleFormat {
    Float32,
    Int24,
}

const BLOCK: usize = 1 << 16;

/// Returns `<dir>/<stem>.<tag>.<ext>`, adding ` (2)`, ` (3)`… so that an
/// existing file is never overwritten.
pub fn unique_output_path(input: &Path, tag: &str, ext: &str) -> PathBuf {
    let dir = input.parent().unwrap_or(Path::new("."));
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "export".into());
    let mut n = 1;
    loop {
        let name = if n == 1 { format!("{stem}.{tag}.{ext}") } else { format!("{stem}.{tag} ({n}).{ext}") };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Writes one track with attenuation applied, all channels, native rate.
pub fn export_wav(
    track: &ExportTrack<'_>,
    path: &Path,
    format: WavSampleFormat,
    progress: &Progress,
) -> Result<(), MediaError> {
    let store = track.store;
    let channels = store.n_channels();
    let spec = hound::WavSpec {
        channels: channels as u16,
        sample_rate: store.rate(),
        bits_per_sample: if format == WavSampleFormat::Float32 { 32 } else { 24 },
        sample_format: if format == WavSampleFormat::Float32 {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let tmp = path.with_extension("wav.part");
    let mut writer = hound::WavWriter::create(&tmp, spec).map_err(wav_err)?;
    let mut planes = vec![vec![0.0f32; BLOCK]; channels];
    let mut scratch = Vec::new();
    let len = store.len();
    let mut pos = 0usize;
    while pos < len {
        if progress.cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = std::fs::remove_file(&tmp);
            return Err(MediaError::Cancelled);
        }
        let n = BLOCK.min(len - pos);
        for (ch, plane) in planes.iter_mut().enumerate() {
            render_source_block(store, ch, Some(track.envelope), pos as i64, &mut plane[..n], &mut scratch);
        }
        for i in 0..n {
            for plane in &planes {
                match format {
                    WavSampleFormat::Float32 => writer.write_sample(plane[i]).map_err(wav_err)?,
                    WavSampleFormat::Int24 => writer.write_sample(to_i24(plane[i])).map_err(wav_err)?,
                }
            }
        }
        pos += n;
        progress.set(pos as f64 / len as f64);
    }
    writer.finalize().map_err(wav_err)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn to_i24(x: f32) -> i32 {
    (f64::from(x) * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32
}

fn wav_err(e: hound::Error) -> MediaError {
    match e {
        hound::Error::IoError(io) => MediaError::Io(io),
        other => MediaError::Other(format!("WAV: {other}")),
    }
}

enum Plan {
    Copy { out: usize, in_tb: ff::Rational },
    Encode,
}

struct TrackEncoder<'a> {
    track: &'a ExportTrack<'a>,
    encoder: ff::encoder::Audio,
    out: usize,
    out_tb: ff::Rational,
    rate: u32,
    frame_size: usize,
    /// Timeline offset of sample 0, in samples (container start time).
    pts_offset: i64,
    next: usize,
    planes: Vec<Vec<f32>>,
    scratch: Vec<f32>,
    done: bool,
}

/// Creates `output` from `input`: tracks listed in `tracks` are replaced by
/// their processed audio (same codec where an encoder is available);
/// all other streams, stream metadata and container metadata are copied
/// without re-encoding.
pub fn remux(input: &Path, output: &Path, tracks: &[ExportTrack<'_>], progress: &Progress) -> Result<(), MediaError> {
    crate::init();
    let mut ictx = ff::format::input(input).map_err(|source| MediaError::Open { path: input.to_owned(), source })?;
    let tmp = output.with_file_name(format!(
        ".{}.part",
        output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    // The muxer is chosen from the final extension, not the temp name.
    let format_name = output.extension().and_then(|e| e.to_str()).map(muxer_for_extension);
    let mut octx = match format_name {
        Some(name) => ff::format::output_as(&tmp, name),
        None => ff::format::output(&tmp),
    }?;
    let global_header = octx.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);
    // SAFETY: plain field read from a valid context.
    let start_time = unsafe { (*ictx.as_ptr()).start_time };
    let global_start = if start_time == ffi::AV_NOPTS_VALUE { 0.0 } else { start_time as f64 / f64::from(ffi::AV_TIME_BASE) };
    let duration = tracks.iter().map(|t| t.store.duration_secs()).fold(0.0, f64::max).max(1e-3);

    let mut plans: Vec<Option<Plan>> = Vec::new();
    let mut encoders: Vec<TrackEncoder<'_>> = Vec::new();
    for ist in ictx.streams() {
        let out = octx.nb_streams() as usize;
        let params = ist.parameters();
        if let Some(track) = tracks.iter().find(|t| t.id.0 as usize == ist.index()) {
            let rate = track.store.rate();
            let channels = track.store.n_channels();
            let src_bits = unsafe { (*params.as_ptr()).bits_per_raw_sample };
            let src_bit_rate = params.bit_rate();
            let codec = choose_encoder(params.id())?;
            let mut ost = octx.add_stream(codec)?;
            let ctx = ff::codec::context::Context::new_with_codec(codec);
            let mut enc = ctx.encoder().audio()?;
            enc.set_rate(rate as i32);
            enc.set_format(choose_sample_format(&codec, src_bits)?);
            enc.set_time_base(ff::Rational::new(1, rate as i32));
            // SAFETY: configuring fields of an unopened codec context.
            unsafe {
                let raw = enc.as_mut_ptr();
                ffi::av_channel_layout_uninit(&mut (*raw).ch_layout);
                ffi::av_channel_layout_default(&mut (*raw).ch_layout, channels as i32);
                if src_bits > 0 {
                    (*raw).bits_per_raw_sample = src_bits;
                }
            }
            if is_lossy(codec.id()) {
                // Never go below the source bitrate; sensible floor otherwise.
                let floor = 96_000 * channels.max(1);
                enc.set_bit_rate((src_bit_rate.max(0) as usize).max(floor));
            }
            if global_header {
                enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
            }
            let encoder = enc.open_as(codec)?;
            ost.set_parameters(&encoder);
            ost.set_time_base(ff::Rational::new(1, rate as i32));
            ost.set_metadata(ist.metadata().to_owned());
            copy_disposition(&ist, &mut ost);
            let frame_size = match encoder.frame_size() {
                0 => 4096,
                n => n as usize,
            };
            encoders.push(TrackEncoder {
                track,
                encoder,
                out,
                out_tb: ff::Rational::new(1, rate as i32),
                rate,
                frame_size,
                pts_offset: (global_start * f64::from(rate)).round() as i64,
                next: 0,
                planes: vec![vec![0.0; frame_size]; channels],
                scratch: Vec::new(),
                done: false,
            });
            plans.push(Some(Plan::Encode));
        } else {
            let mut ost = octx.add_stream(ff::encoder::find(ff::codec::Id::None))?;
            ost.set_parameters(params);
            // Let the muxer pick a tag valid for the output container.
            unsafe { (*(*ost.as_mut_ptr()).codecpar).codec_tag = 0 };
            ost.set_time_base(ist.time_base());
            ost.set_metadata(ist.metadata().to_owned());
            copy_disposition(&ist, &mut ost);
            plans.push(Some(Plan::Copy { out, in_tb: ist.time_base() }));
        }
    }
    octx.set_metadata(ictx.metadata().to_owned());
    octx.write_header()?;
    for e in &mut encoders {
        e.out_tb = octx.stream(e.out).map(|s| s.time_base()).unwrap_or(e.out_tb);
    }

    let result = (|| -> Result<(), MediaError> {
        for (ist, mut packet) in ictx.packets() {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err(MediaError::Cancelled);
            }
            let Some(Some(Plan::Copy { out, in_tb })) = plans.get(ist.index()) else { continue };
            // Keep audio ~0.5 s ahead of copied streams so the muxer can
            // interleave without buffering large amounts of data.
            if let Some(ts) = packet.dts().or(packet.pts()) {
                let t = ts as f64 * f64::from(*in_tb) - global_start;
                for e in &mut encoders {
                    e.encode_until(t + 0.5, &mut octx)?;
                }
                progress.set(0.99 * t / duration);
            }
            let out_tb = octx.stream(*out).map(|s| s.time_base()).unwrap_or(*in_tb);
            packet.rescale_ts(*in_tb, out_tb);
            packet.set_stream(*out);
            packet.set_position(-1);
            packet.write_interleaved(&mut octx)?;
        }
        for e in &mut encoders {
            e.encode_until(f64::INFINITY, &mut octx)?;
            e.finish(&mut octx)?;
        }
        octx.write_trailer()?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            drop(octx);
            std::fs::rename(&tmp, output)?;
            progress.set(1.0);
            Ok(())
        }
        Err(e) => {
            drop(octx);
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

impl TrackEncoder<'_> {
    /// Encodes processed audio up to timeline time `t` (seconds).
    fn encode_until(&mut self, t: f64, octx: &mut ff::format::context::Output) -> Result<(), MediaError> {
        let len = self.track.store.len();
        let target = if t.is_finite() { ((t * f64::from(self.rate)) as usize).min(len) } else { len };
        while !self.done && self.next < target {
            let n = self.frame_size.min(len - self.next);
            for (ch, plane) in self.planes.iter_mut().enumerate() {
                plane.resize(n, 0.0);
                render_source_block(
                    self.track.store,
                    ch,
                    Some(self.track.envelope),
                    self.next as i64,
                    plane,
                    &mut self.scratch,
                );
            }
            let frame = make_frame(&self.planes, self.encoder.format(), self.rate, self.pts_offset + self.next as i64)?;
            self.encoder.send_frame(&frame)?;
            self.next += n;
            self.drain(octx)?;
        }
        Ok(())
    }

    fn finish(&mut self, octx: &mut ff::format::context::Output) -> Result<(), MediaError> {
        if !self.done {
            self.encoder.send_eof()?;
            self.drain(octx)?;
            self.done = true;
        }
        Ok(())
    }

    fn drain(&mut self, octx: &mut ff::format::context::Output) -> Result<(), MediaError> {
        let mut packet = ff::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    packet.set_stream(self.out);
                    packet.rescale_ts(ff::Rational::new(1, self.rate as i32), self.out_tb);
                    packet.write_interleaved(octx)?;
                }
                Err(ff::Error::Eof) => return Ok(()),
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn make_frame(planes: &[Vec<f32>], format: Sample, rate: u32, pts: i64) -> Result<ff::frame::Audio, MediaError> {
    let n = planes.first().map_or(0, Vec::len);
    let channels = planes.len();
    let mut frame = ff::frame::Audio::empty();
    // SAFETY: we set the fields av_frame_get_buffer needs, then allocate.
    unsafe {
        let raw = frame.as_mut_ptr();
        (*raw).format = ffi::AVSampleFormat::from(format) as i32;
        (*raw).nb_samples = n as i32;
        (*raw).sample_rate = rate as i32;
        ffi::av_channel_layout_default(&mut (*raw).ch_layout, channels as i32);
        if ffi::av_frame_get_buffer(raw, 0) < 0 {
            return Err(MediaError::Other("nie można przydzielić ramki audio".into()));
        }
    }
    frame.set_pts(Some(pts));
    macro_rules! fill {
        ($ty:ty, $conv:expr) => {{
            let conv = $conv;
            if format.is_planar() {
                for (ch, src) in planes.iter().enumerate() {
                    let dst: &mut [$ty] = frame.plane_mut(ch);
                    for (d, s) in dst.iter_mut().zip(src) {
                        *d = conv(*s);
                    }
                }
            } else {
                let dst: &mut [$ty] = frame.plane_mut(0);
                for (i, chunk) in dst.chunks_exact_mut(channels).enumerate().take(n) {
                    for (d, src) in chunk.iter_mut().zip(planes) {
                        *d = conv(src[i]);
                    }
                }
            }
        }};
    }
    match format {
        Sample::F32(_) => fill!(f32, |x: f32| x),
        Sample::F64(_) => fill!(f64, |x: f32| f64::from(x)),
        Sample::I16(_) => fill!(i16, |x: f32| (f64::from(x) * 32768.0).round().clamp(-32768.0, 32767.0) as i16),
        Sample::I32(_) => fill!(i32, |x: f32| (f64::from(x) * 2_147_483_648.0).round().clamp(-2_147_483_648.0, 2_147_483_647.0) as i32),
        Sample::U8(_) => fill!(u8, |x: f32| (f64::from(x) * 128.0 + 128.0).round().clamp(0.0, 255.0) as u8),
        other => return Err(MediaError::UnsupportedSampleFormat(format!("{other:?}"))),
    }
    Ok(frame)
}

fn choose_encoder(id: ff::codec::Id) -> Result<ff::Codec, MediaError> {
    use ff::codec::Id;
    let by_name = |n: &str| ff::encoder::find_by_name(n);
    let found = match id {
        Id::OPUS => by_name("libopus").or_else(|| ff::encoder::find(id)),
        Id::MP3 => by_name("libmp3lame"),
        Id::VORBIS => by_name("libvorbis").or_else(|| ff::encoder::find(id)),
        Id::AAC => by_name("aac"),
        _ => ff::encoder::find(id),
    };
    // Unknown or decode-only codecs fall back to lossless FLAC.
    found
        .or_else(|| ff::encoder::find(Id::FLAC))
        .ok_or_else(|| MediaError::Other("brak kodera audio w FFmpeg".into()))
}

fn is_lossy(id: ff::codec::Id) -> bool {
    use ff::codec::Id;
    !matches!(
        id,
        Id::FLAC | Id::ALAC | Id::WAVPACK | Id::TTA | Id::TRUEHD | Id::MLP
            | Id::PCM_S16LE | Id::PCM_S16BE | Id::PCM_S24LE | Id::PCM_S24BE | Id::PCM_S32LE
            | Id::PCM_S32BE | Id::PCM_F32LE | Id::PCM_F32BE | Id::PCM_F64LE | Id::PCM_U8
    )
}

/// Picks the encoder sample format that preserves the source precision:
/// integer formats matching the source bit depth when available, float
/// otherwise.
fn choose_sample_format(codec: &ff::Codec, src_bits: i32) -> Result<Sample, MediaError> {
    let formats: Vec<Sample> = codec.audio()?.formats().map(|f| f.collect()).unwrap_or_default();
    if formats.is_empty() {
        return Ok(Sample::F32(SampleLayout::Planar));
    }
    let wants_16 = src_bits > 0 && src_bits <= 16;
    let preference: &[Sample] = if wants_16 {
        &[Sample::I16(SampleLayout::Packed), Sample::I16(SampleLayout::Planar), Sample::I32(SampleLayout::Packed), Sample::I32(SampleLayout::Planar)]
    } else {
        &[
            Sample::F32(SampleLayout::Planar),
            Sample::F32(SampleLayout::Packed),
            Sample::I32(SampleLayout::Packed),
            Sample::I32(SampleLayout::Planar),
            Sample::I16(SampleLayout::Packed),
            Sample::I16(SampleLayout::Planar),
        ]
    };
    Ok(preference.iter().copied().find(|f| formats.contains(f)).unwrap_or(formats[0]))
}

fn copy_disposition(ist: &ff::format::stream::Stream<'_>, ost: &mut ff::format::stream::StreamMut<'_>) {
    // SAFETY: plain field copy between valid streams.
    unsafe { (*ost.as_mut_ptr()).disposition = (*ist.as_ptr()).disposition };
}

fn muxer_for_extension(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "mkv" => "matroska",
        "webm" => "webm",
        "mov" => "mov",
        "mp4" | "m4v" => "mp4",
        "m4a" => "ipod",
        "avi" => "avi",
        "mts" | "m2ts" | "ts" => "mpegts",
        "wav" => "wav",
        "flac" => "flac",
        "mp3" => "mp3",
        _ => "matroska",
    }
}
