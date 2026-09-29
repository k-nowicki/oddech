//! Import pipeline: probe → (cache hit | decode into cache) → open stores.
//!
//! After import every track is available as a memory-mapped
//! [`TrackStore`] plus a [`WaveformPyramid`] for drawing.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use oddech_core::cache::{CACHE_FORMAT_VERSION, CacheEntry, CachedTrack, Manifest};
use oddech_core::pyramid::{PyramidBuilder, WaveformPyramid};
use oddech_core::store::{TrackStore, TrackStoreWriter};
use oddech_core::TrackInfo;

use crate::decode::{PlanarSink, decode_tracks};
use crate::probe::{MediaInfo, probe};
use crate::MediaError;

/// Lock-free progress of a background job, shared with the UI.
#[derive(Debug, Default)]
pub struct ImportProgress {
    /// Fraction done in `[0, 1]`, stored as `f64` bits.
    fraction: AtomicU64,
    pub cancel: AtomicBool,
}

impl ImportProgress {
    pub fn fraction(&self) -> f64 {
        f64::from_bits(self.fraction.load(Ordering::Relaxed))
    }

    pub fn set(&self, f: f64) {
        self.fraction.store(f.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct ImportedTrack {
    pub info: TrackInfo,
    pub store: TrackStore,
    pub pyramid: WaveformPyramid,
}

#[derive(Debug)]
pub struct ImportedMedia {
    pub info: MediaInfo,
    pub cache: CacheEntry,
    pub tracks: Vec<ImportedTrack>,
    pub from_cache: bool,
}

/// Imports `path`, using the cache under `cache_root` when possible.
pub fn import_media(
    path: &Path,
    cache_root: &Path,
    progress: &Arc<ImportProgress>,
) -> Result<ImportedMedia, MediaError> {
    let mut info = probe(path)?;
    let cache = CacheEntry::for_media(cache_root, path)?;

    let result = import_into(path, &cache, &mut info, progress);
    if result.is_ok() {
        cache.touch();
    }
    result.map(|(tracks, from_cache)| ImportedMedia { info, cache, tracks, from_cache })
}

fn import_into(
    path: &Path,
    cache: &CacheEntry,
    info: &mut MediaInfo,
    progress: &Arc<ImportProgress>,
) -> Result<(Vec<ImportedTrack>, bool), MediaError> {
    if let Some(manifest) = cache.load_manifest() {
        match open_cached(cache, &manifest) {
            Ok(tracks) => {
                log::info!("cache hit for {}", path.display());
                apply_exact_lengths(info, &tracks);
                progress.set(1.0);
                return Ok((tracks, true));
            }
            Err(e) => log::warn!("cache entry unusable, re-decoding: {e}"),
        }
    }

    cache.reset()?;
    let sinks: Vec<Box<dyn PlanarSink>> = info
        .tracks
        .iter()
        .map(|t| -> Result<Box<dyn PlanarSink>, MediaError> {
            Ok(Box::new(CacheSink::create(cache, t)?))
        })
        .collect::<Result<_, _>>()?;

    let duration = info.duration_secs.max(1e-3);
    let prog = Arc::clone(progress);
    let lens = decode_tracks(
        path,
        &info.tracks,
        sinks,
        &move |t| prog.set(0.98 * t / duration),
        &progress.cancel,
    )?;

    let manifest = Manifest {
        version: CACHE_FORMAT_VERSION,
        source_path: path.to_owned(),
        tracks: info
            .tracks
            .iter()
            .zip(&lens)
            .map(|(t, &len)| CachedTrack { info: t.clone(), len_samples: len })
            .collect(),
    };
    let tracks = open_cached(cache, &manifest)?;
    cache.commit(&manifest)?;
    apply_exact_lengths(info, &tracks);
    progress.set(1.0);
    Ok((tracks, false))
}

/// Replaces container-reported durations with exact decoded lengths.
fn apply_exact_lengths(info: &mut MediaInfo, tracks: &[ImportedTrack]) {
    for (t, imported) in info.tracks.iter_mut().zip(tracks) {
        t.duration_secs = imported.store.duration_secs();
    }
    info.duration_secs = tracks.iter().map(|t| t.store.duration_secs()).fold(0.0, f64::max);
}

fn open_cached(cache: &CacheEntry, manifest: &Manifest) -> Result<Vec<ImportedTrack>, MediaError> {
    manifest
        .tracks
        .iter()
        .map(|ct| {
            let info = ct.info.clone();
            let store = TrackStore::open(
                &cache.track_dir(info.id),
                usize::from(info.channels.max(1)),
                info.sample_rate,
            )?;
            if store.len() as u64 != ct.len_samples {
                return Err(MediaError::Other("niezgodna długość w cache".into()));
            }
            let mut r = BufReader::new(File::open(cache.pyramid_path(info.id))?);
            let pyramid = WaveformPyramid::read_from(&mut r)?;
            Ok(ImportedTrack { info, store, pyramid })
        })
        .collect()
}

/// Writes decoded samples to the store and builds the display pyramid of
/// the channel mix on the fly.
struct CacheSink {
    writer: TrackStoreWriter,
    pyramid: PyramidBuilder,
    pyramid_path: std::path::PathBuf,
    mix: Vec<f32>,
}

impl CacheSink {
    fn create(cache: &CacheEntry, track: &TrackInfo) -> Result<Self, MediaError> {
        let dir = cache.track_dir(track.id);
        let expected = (track.duration_secs * f64::from(track.sample_rate)) as u64;
        Ok(Self {
            writer: TrackStoreWriter::create(&dir, usize::from(track.channels.max(1)))?,
            pyramid: PyramidBuilder::with_capacity_samples(expected),
            pyramid_path: cache.pyramid_path(track.id),
            mix: Vec::new(),
        })
    }
}

impl PlanarSink for CacheSink {
    fn push(&mut self, planes: &[&[f32]]) -> std::io::Result<()> {
        self.writer.push_planar(planes)?;
        let n = planes.first().map_or(0, |p| p.len());
        let pyramid = &mut self.pyramid;
        if planes.len() == 1 {
            pyramid.push(planes[0]);
        } else {
            self.mix.clear();
            self.mix.resize(n, 0.0);
            let scale = 1.0 / planes.len() as f32;
            for p in planes {
                for (m, x) in self.mix.iter_mut().zip(p.iter()) {
                    *m += x * scale;
                }
            }
            pyramid.push(&self.mix);
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> std::io::Result<()> {
        let this = *self;
        this.writer.finish()?;
        let pyramid = this.pyramid.finish();
        let mut w = BufWriter::new(File::create(&this.pyramid_path)?);
        pyramid.write_to(&mut w)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_data()
    }
}
