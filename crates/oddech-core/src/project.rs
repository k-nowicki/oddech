//! The project file (`<video>.oddech.json`) stored next to the media file.
//!
//! It holds everything the user decided: which tracks are fixed, regions and
//! review decisions, attenuation settings. Loading a video automatically
//! restores its project.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gain::DuckParams;
use crate::region::{Discrimination, Gate, Region, RegionId};
use crate::track::TrackId;

pub const PROJECT_VERSION: u32 = 1;
/// Minimum model confidence for a region to be attenuated (an application
/// setting, not per project). The UI shows "Czułość" = 1 − threshold.
pub const DEFAULT_THRESHOLD: f32 = 0.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackSettings {
    /// "Popraw wdechy": detect and attenuate breaths on this track.
    pub fix: bool,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub duck: DuckParams,
    /// Detection has run on this track (so switching "fix" on again does
    /// not re-run it automatically).
    #[serde(default)]
    pub detected: bool,
}

impl Default for TrackSettings {
    fn default() -> Self {
        Self { fix: false, mute: false, solo: false, duck: DuckParams::default(), detected: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    /// File name of the media (informational; the project lives beside it).
    pub media_file: String,
    #[serde(default)]
    pub tracks: BTreeMap<TrackId, TrackSettings>,
    #[serde(default)]
    pub regions: Vec<Region>,
    #[serde(default)]
    pub discrimination: Discrimination,
    #[serde(default)]
    next_region_id: u64,
}


impl Project {
    pub fn new(media_path: &Path) -> Self {
        Self {
            version: PROJECT_VERSION,
            media_file: media_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            tracks: BTreeMap::new(),
            regions: Vec::new(),
            discrimination: Discrimination::default(),
            next_region_id: 1,
        }
    }

    /// `movie.mkv` → `movie.mkv.oddech.json`
    pub fn path_for(media_path: &Path) -> PathBuf {
        let mut name = media_path.as_os_str().to_owned();
        name.push(".oddech.json");
        PathBuf::from(name)
    }

    /// Read-only snapshot of the user's labels, kept next to the media
    /// and refreshed only on an explicit "save all".
    pub fn reference_path_for(media_path: &Path) -> PathBuf {
        let mut name = media_path.as_os_str().to_owned();
        name.push(".oddech.reference.json");
        PathBuf::from(name)
    }

    /// Writes a read-only copy (replacing an older one atomically).
    pub fn save_read_only(&self, path: &Path) -> io::Result<()> {
        self.save(path)?;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_readonly(true);
        fs::set_permissions(path, perms)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let mut p: Self = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if p.version > PROJECT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("project version {} is newer than supported {PROJECT_VERSION}", p.version),
            ));
        }
        // Guard against hand-edited files with duplicate or stale ids.
        let max_id = p.regions.iter().map(|r| r.id.0).max().unwrap_or(0);
        p.next_region_id = p.next_region_id.max(max_id + 1);
        p.regions.retain(|r| r.end > r.start && r.start.is_finite() && r.end.is_finite());
        Ok(p)
    }

    /// Atomic save: write to a temporary file, then rename over the target.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("json.part");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, path)
    }

    /// What decides whether model regions are active, given the
    /// application's confidence threshold.
    pub fn gate(&self, threshold: f32) -> Gate {
        Gate { threshold, discrimination: self.discrimination }
    }

    pub fn track(&self, id: TrackId) -> TrackSettings {
        self.tracks.get(&id).cloned().unwrap_or_default()
    }

    pub fn track_mut(&mut self, id: TrackId) -> &mut TrackSettings {
        self.tracks.entry(id).or_default()
    }

    pub fn allocate_region_id(&mut self) -> RegionId {
        let id = RegionId(self.next_region_id.max(1));
        self.next_region_id = id.0 + 1;
        id
    }

    pub fn region(&self, id: RegionId) -> Option<&Region> {
        self.regions.iter().find(|r| r.id == id)
    }

    pub fn region_mut(&mut self, id: RegionId) -> Option<&mut Region> {
        self.regions.iter_mut().find(|r| r.id == id)
    }

    pub fn remove_region(&mut self, id: RegionId) -> Option<Region> {
        let idx = self.regions.iter().position(|r| r.id == id)?;
        Some(self.regions.remove(idx))
    }

    /// Regions of one track, sorted by start time.
    pub fn track_regions(&self, track: TrackId) -> Vec<&Region> {
        let mut v: Vec<&Region> = self.regions.iter().filter(|r| r.track == track).collect();
        v.sort_by(|a, b| a.start.total_cmp(&b.start));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::{RegionKind, RegionSource};

    #[test]
    fn read_only_reference_can_be_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("a.mkv");
        let path = Project::reference_path_for(&media);
        let mut p = Project::new(&media);
        p.save_read_only(&path).unwrap();
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        p.discrimination.distance_s = 3;
        p.save_read_only(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap().discrimination.distance_s, 3);
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("film.mkv");
        let mut p = Project::new(&media);
        p.track_mut(TrackId(2)).fix = true;
        p.track_mut(TrackId(2)).duck.gain_db = f32::NEG_INFINITY;
        let id = p.allocate_region_id();
        p.regions.push(Region {
            id,
            track: TrackId(2),
            start: 1.25,
            end: 1.75,
            kind: RegionKind::Breath,
            source: RegionSource::Model,
            confidence: Some(0.8),
            gain_db: Some(-30.0),
            fade_ms: None,
            silence_db: Vec::new(),
        });
        let path = Project::path_for(&media);
        assert!(path.to_string_lossy().ends_with("film.mkv.oddech.json"));
        p.save(&path).unwrap();
        let q = Project::load(&path).unwrap();
        assert_eq!(p, q);

        let mut q = q;
        assert!(q.allocate_region_id().0 > id.0);
    }

    #[test]
    fn rejects_future_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.oddech.json");
        fs::write(&path, r#"{"version": 999, "media_file": "x"}"#).unwrap();
        assert!(Project::load(&path).is_err());
    }
}
