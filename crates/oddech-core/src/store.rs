//! Disk-backed storage of decoded samples.
//!
//! Decoded audio is written once, as one little-endian `f32` file per
//! channel, and then memory-mapped read-only. The OS page cache decides what
//! stays resident, so a 90-minute, 6-track session does not need gigabytes
//! of heap, and a second open of the same file costs nothing but `mmap`.
//! The mapped data is never modified: attenuation is always applied on the
//! fly, which keeps the source buffers bit-identical to the decode.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;

/// Read-only, memory-mapped samples of one track.
#[derive(Debug)]
pub struct TrackStore {
    rate: u32,
    len: usize,
    channels: Vec<Option<Mmap>>,
}

fn channel_path(dir: &Path, ch: usize) -> PathBuf {
    dir.join(format!("ch{ch}.f32"))
}

impl TrackStore {
    /// Opens a store previously produced by [`TrackStoreWriter::finish`].
    pub fn open(dir: &Path, n_channels: usize, rate: u32) -> io::Result<Self> {
        let mut channels = Vec::with_capacity(n_channels);
        let mut len = None;
        for ch in 0..n_channels {
            let file = File::open(channel_path(dir, ch))?;
            let bytes = file.metadata()?.len() as usize;
            if bytes % 4 != 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated sample file"));
            }
            let n = bytes / 4;
            if *len.get_or_insert(n) != n {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "channel length mismatch"));
            }
            // Zero-length files cannot be mapped.
            channels.push(if n == 0 {
                None
            } else {
                // SAFETY: the cache files are private to this application and
                // never modified after `finish` renamed them into place.
                let map = unsafe { Mmap::map(&file)? };
                #[cfg(unix)]
                let _ = map.advise(memmap2::Advice::Sequential);
                Some(map)
            });
        }
        Ok(Self { rate, len: len.unwrap_or(0), channels })
    }

    /// An in-memory store; convenient for tests and synthetic data.
    #[doc(hidden)]
    pub fn from_vecs(dir: &Path, rate: u32, data: &[Vec<f32>]) -> io::Result<Self> {
        let mut w = TrackStoreWriter::create(dir, data.len())?;
        let refs: Vec<&[f32]> = data.iter().map(Vec::as_slice).collect();
        w.push_planar(&refs)?;
        w.finish()?;
        Self::open(dir, data.len(), rate)
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Number of samples per channel.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn n_channels(&self) -> usize {
        self.channels.len()
    }

    pub fn duration_secs(&self) -> f64 {
        self.len as f64 / f64::from(self.rate)
    }

    /// The whole channel as a slice.
    pub fn channel(&self, ch: usize) -> &[f32] {
        match &self.channels[ch] {
            Some(map) => bytemuck::cast_slice(&map[..]),
            None => &[],
        }
    }

    /// Copies samples `[start, start + out.len())` of channel `ch` into `out`,
    /// zero-filling anything outside the signal.
    pub fn read(&self, ch: usize, start: i64, out: &mut [f32]) {
        let src = self.channel(ch);
        copy_clamped(src, start, out);
    }

    /// One channel (`Some(ch)`) or the mono mix (`None`).
    pub fn read_view(&self, channel: Option<usize>, start: i64, out: &mut [f32]) {
        match channel {
            Some(ch) => self.read(ch, start, out),
            None => self.read_mix(start, out),
        }
    }

    /// Mono mix (channel average) of `[start, start + out.len())`.
    pub fn read_mix(&self, start: i64, out: &mut [f32]) {
        let n = self.n_channels();
        if n == 1 {
            return self.read(0, start, out);
        }
        out.fill(0.0);
        let scale = 1.0 / n as f32;
        for ch in 0..n {
            let src = self.channel(ch);
            let (dst_off, src_range) = clamp_range(src.len(), start, out.len());
            for (o, x) in out[dst_off..].iter_mut().zip(&src[src_range]) {
                *o += x * scale;
            }
        }
    }
}

/// Maps a request window onto a source of length `len`: returns the offset
/// into the destination where valid data starts and the source range.
fn clamp_range(len: usize, start: i64, count: usize) -> (usize, std::ops::Range<usize>) {
    let s = start.clamp(0, len as i64) as usize;
    let e = (start + count as i64).clamp(0, len as i64) as usize;
    let dst_off = (s as i64 - start).max(0) as usize;
    (dst_off, s..e.max(s))
}

fn copy_clamped(src: &[f32], start: i64, out: &mut [f32]) {
    let (dst_off, range) = clamp_range(src.len(), start, out.len());
    let n = range.len();
    out[..dst_off].fill(0.0);
    out[dst_off..dst_off + n].copy_from_slice(&src[range]);
    out[dst_off + n..].fill(0.0);
}

/// Streams planar samples to disk. Files are written under a temporary name
/// and renamed on [`finish`](Self::finish), so an interrupted decode never
/// leaves a store that looks complete.
pub struct TrackStoreWriter {
    dir: PathBuf,
    writers: Vec<BufWriter<File>>,
}

impl TrackStoreWriter {
    pub fn create(dir: &Path, n_channels: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let writers = (0..n_channels)
            .map(|ch| {
                let tmp = channel_path(dir, ch).with_extension("f32.part");
                File::create(tmp).map(|f| BufWriter::with_capacity(1 << 20, f))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self { dir: dir.to_owned(), writers })
    }

    /// Appends one block; all channel slices must have the same length.
    pub fn push_planar(&mut self, channels: &[&[f32]]) -> io::Result<()> {
        debug_assert_eq!(channels.len(), self.writers.len());
        for (w, data) in self.writers.iter_mut().zip(channels) {
            w.write_all(bytemuck::cast_slice(data))?;
        }
        Ok(())
    }

    pub fn finish(self) -> io::Result<()> {
        for (ch, w) in self.writers.into_iter().enumerate() {
            let file = w.into_inner().map_err(|e| e.into_error())?;
            file.sync_data()?;
            let final_path = channel_path(&self.dir, ch);
            fs::rename(final_path.with_extension("f32.part"), final_path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_open_read() {
        let dir = tempfile::tempdir().unwrap();
        let l: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let r: Vec<f32> = (0..1000).map(|i| -(i as f32)).collect();
        let mut w = TrackStoreWriter::create(dir.path(), 2).unwrap();
        w.push_planar(&[&l[..400], &r[..400]]).unwrap();
        w.push_planar(&[&l[400..], &r[400..]]).unwrap();
        w.finish().unwrap();

        let s = TrackStore::open(dir.path(), 2, 48_000).unwrap();
        assert_eq!(s.len(), 1000);
        assert_eq!(s.channel(0), &l[..]);

        let mut buf = [9.0f32; 6];
        s.read(0, -2, &mut buf);
        assert_eq!(buf, [0.0, 0.0, 0.0, 1.0, 2.0, 3.0]);
        s.read(1, 997, &mut buf);
        assert_eq!(buf, [-997.0, -998.0, -999.0, 0.0, 0.0, 0.0]);
        s.read_mix(10, &mut buf);
        assert_eq!(buf, [0.0; 6]);
        s.read(0, 5000, &mut buf);
        assert_eq!(buf, [0.0; 6]);
    }

    #[test]
    fn unfinished_store_is_not_openable() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = TrackStoreWriter::create(dir.path(), 1).unwrap();
        w.push_planar(&[&[1.0, 2.0]]).unwrap();
        drop(w);
        assert!(TrackStore::open(dir.path(), 1, 48_000).is_err());
    }
}
