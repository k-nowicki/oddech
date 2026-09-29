//! On-disk cache of decoded audio and derived data.
//!
//! Layout: `~/.cache/oddech/<key>/` where `<key>` is a fingerprint of the
//! media file. Inside, each track has a subdirectory with its sample store
//! and waveform pyramid; `manifest.json` is written last and marks the entry
//! as complete.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::track::{TrackId, TrackInfo};

/// Bump whenever the on-disk format of anything in the cache changes.
pub const CACHE_FORMAT_VERSION: u32 = 1;

/// Default cache root (`~/.cache/oddech`), overridable with `ODDECH_CACHE_DIR`.
pub fn default_cache_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("ODDECH_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("", "", "oddech")
        .map(|d| d.cache_dir().to_owned())
        .unwrap_or_else(|| std::env::temp_dir().join("oddech-cache"))
}

/// Fingerprint of a media file that is cheap to compute even for multi-GB
/// files: size, modification time and a hash of the first and last MiB.
/// Hashing the whole file would cost seconds and defeat the "second open in
/// under 2 s" goal; the combination above is collision-safe in practice for
/// a personal tool.
pub fn fingerprint(path: &Path) -> io::Result<String> {
    const WINDOW: u64 = 1 << 20;
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());

    let mut hasher = blake3::Hasher::new();
    hasher.update(&CACHE_FORMAT_VERSION.to_le_bytes());
    hasher.update(&size.to_le_bytes());
    hasher.update(&mtime.to_le_bytes());
    let mut buf = vec![0u8; WINDOW as usize];
    let head = read_up_to(&mut file, &mut buf)?;
    hasher.update(&buf[..head]);
    if size > WINDOW {
        file.seek(SeekFrom::Start(size.saturating_sub(WINDOW).max(WINDOW)))?;
        let tail = read_up_to(&mut file, &mut buf)?;
        hasher.update(&buf[..tail]);
    }
    Ok(hasher.finalize().to_hex()[..32].to_owned())
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Contents of `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub source_path: PathBuf,
    pub tracks: Vec<CachedTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedTrack {
    pub info: TrackInfo,
    /// Exact number of decoded samples per channel.
    pub len_samples: u64,
}

/// One cache entry directory.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    dir: PathBuf,
}

impl CacheEntry {
    pub fn for_media(root: &Path, path: &Path) -> io::Result<Self> {
        Ok(Self { dir: root.join(fingerprint(path)?) })
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn track_dir(&self, id: TrackId) -> PathBuf {
        self.dir.join(format!("track{}", id.0))
    }

    pub fn pyramid_path(&self, id: TrackId) -> PathBuf {
        self.track_dir(id).join("mix.wpyr")
    }

    /// Per-channel pyramids, built on demand for the split-channel view.
    pub fn channel_pyramid_path(&self, id: TrackId, ch: usize) -> PathBuf {
        self.track_dir(id).join(format!("ch{ch}.wpyr"))
    }

    /// Directory for cached analysis results (transcripts, detections).
    pub fn analysis_dir(&self) -> PathBuf {
        self.dir.join("analysis")
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    /// Loads the manifest if the entry is complete and of the current version.
    pub fn load_manifest(&self) -> Option<Manifest> {
        let text = fs::read_to_string(self.manifest_path()).ok()?;
        let m: Manifest = serde_json::from_str(&text).ok()?;
        (m.version == CACHE_FORMAT_VERSION).then_some(m)
    }

    /// Writes the manifest atomically; call only after all data is on disk.
    pub fn commit(&self, manifest: &Manifest) -> io::Result<()> {
        let tmp = self.manifest_path().with_extension("json.part");
        fs::write(&tmp, serde_json::to_vec_pretty(manifest)?)?;
        fs::rename(tmp, self.manifest_path())
    }

    /// Removes a partial or stale entry before re-populating it.
    pub fn reset(&self) -> io::Result<()> {
        match fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        fs::create_dir_all(&self.dir)
    }
}

/// Loads per-channel waveform pyramids from the cache, building (and
/// caching) any that are missing.
pub fn channel_pyramids(
    entry: &CacheEntry,
    id: TrackId,
    store: &crate::store::TrackStore,
) -> io::Result<Vec<crate::pyramid::WaveformPyramid>> {
    use crate::pyramid::{WaveformPyramid, build};
    (0..store.n_channels())
        .map(|ch| {
            let path = entry.channel_pyramid_path(id, ch);
            if let Ok(f) = File::open(&path)
                && let Ok(p) = WaveformPyramid::read_from(&mut io::BufReader::new(f))
                && p.len_samples() == store.len() as u64
            {
                return Ok(p);
            }
            let p = build(store.channel(ch));
            let tmp = path.with_extension("wpyr.part");
            let mut w = io::BufWriter::new(File::create(&tmp)?);
            p.write_to(&mut w)?;
            drop(w);
            fs::rename(tmp, &path)?;
            Ok(p)
        })
        .collect()
}

// ------------------------------------------------------------ housekeeping

/// Marker whose modification time says when an entry was last opened.
const LAST_USED: &str = "last_used";

impl CacheEntry {
    /// Records that the entry is in use now (for [`evict`]).
    pub fn touch(&self) {
        if let Err(e) = fs::write(self.dir.join(LAST_USED), b"") {
            log::debug!("cannot mark cache entry {} as used: {e}", self.dir.display());
        }
    }
}

/// One entry as seen by housekeeping.
#[derive(Debug, Clone)]
pub struct EntryUsage {
    pub dir: PathBuf,
    pub bytes: u64,
    pub last_used: std::time::SystemTime,
}

/// Every entry under `root`: directories named by a fingerprint.
pub fn entries(root: &Path) -> Vec<EntryUsage> {
    let Ok(read) = fs::read_dir(root) else { return Vec::new() };
    read.filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .map(|e| {
            let dir = e.path();
            let last_used = [dir.join(LAST_USED), dir.join("manifest.json"), dir.clone()]
                .iter()
                .find_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
                .unwrap_or(UNIX_EPOCH);
            EntryUsage { bytes: dir_size(&dir), dir, last_used }
        })
        .collect()
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(read) = fs::read_dir(dir) else { return 0 };
    read.filter_map(Result::ok)
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

/// Removes least recently used entries until the cache fits in
/// `limit_bytes`. Entries in `keep` (open recordings) are never removed.
/// Returns the bytes freed.
pub fn evict(root: &Path, limit_bytes: u64, keep: &[PathBuf]) -> u64 {
    let mut all = entries(root);
    let mut total: u64 = all.iter().map(|e| e.bytes).sum();
    all.sort_by_key(|e| e.last_used);
    let mut freed = 0;
    for e in all {
        if total <= limit_bytes {
            break;
        }
        if keep.iter().any(|k| k == &e.dir) {
            continue;
        }
        match fs::remove_dir_all(&e.dir) {
            Ok(()) => {
                log::info!("cache: removed {} ({} MB)", e.dir.display(), e.bytes >> 20);
                total -= e.bytes;
                freed += e.bytes;
            }
            Err(err) => log::warn!("cache: cannot remove {}: {err}", e.dir.display()),
        }
    }
    freed
}

/// Removes every entry except those in `keep`. Returns the bytes freed.
pub fn clear(root: &Path, keep: &[PathBuf]) -> u64 {
    evict(root, 0, keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        fs::write(&p, vec![1u8; 3 << 20]).unwrap();
        let a = fingerprint(&p).unwrap();
        assert_eq!(a, fingerprint(&p).unwrap());
        let mut data = vec![1u8; 3 << 20];
        *data.last_mut().unwrap() = 2;
        fs::write(&p, data).unwrap();
        assert_ne!(a, fingerprint(&p).unwrap());
    }

    #[test]
    fn manifest_commit_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let e = CacheEntry::at(dir.path().join("x"));
        e.reset().unwrap();
        assert!(e.load_manifest().is_none());
        let m = Manifest { version: CACHE_FORMAT_VERSION, source_path: "/a.mkv".into(), tracks: vec![] };
        e.commit(&m).unwrap();
        assert!(e.load_manifest().is_some());
    }

    #[test]
    fn eviction_removes_least_recently_used_first() {
        let root = tempfile::tempdir().unwrap();
        let make = |name: &str, bytes: usize, age_s: u64| {
            let dir = root.path().join(name);
            fs::create_dir_all(dir.join("track1")).unwrap();
            fs::write(dir.join("track1").join("data"), vec![0u8; bytes]).unwrap();
            let f = File::create(dir.join(LAST_USED)).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age_s)).unwrap();
            dir
        };
        let old = make(&"a".repeat(32), 1000, 300);
        let open = make(&"b".repeat(32), 1000, 200);
        let recent = make(&"c".repeat(32), 1000, 100);
        fs::create_dir_all(root.path().join("not-an-entry")).unwrap();
        assert_eq!(entries(root.path()).len(), 3);
        // Needs 2 entries' worth of room; the oldest goes, the open one is kept
        // even though it is older than the recent one.
        let freed = evict(root.path(), 1500, &[open.clone()]);
        assert_eq!(freed, 2000);
        assert!(!old.exists() && open.exists() && !recent.exists());
        assert!(root.path().join("not-an-entry").exists());
        assert_eq!(clear(root.path(), &[]), 1000);
    }
}
