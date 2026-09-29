//! An open media file: loaded tracks, the project, playback and view state.
//!
//! The session is the controller between the UI and the lower layers. UI
//! code reads its state and calls its methods; all project mutations go
//! through [`Session::edit`] so undo, autosave and the playback mix stay
//! consistent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oddech_core::cache::CacheEntry;
use oddech_core::gain::GainEnvelope;
use oddech_core::project::Project;
use oddech_core::pyramid::WaveformPyramid;
use oddech_core::store::TrackStore;
use oddech_core::{Region, RegionId, RegionKind, RegionSource, TrackId, TrackInfo};
use oddech_media::{ImportedMedia, MediaInfo};
use oddech_playback::{MixSettings, PlayRequest, PlaybackEngine, TrackMix};

use crate::settings::Settings;
use crate::timeline::TimelineView;

const AUTOSAVE_DELAY: Duration = Duration::from_millis(800);
const UNDO_LIMIT: usize = 200;

pub struct LoadedTrack {
    pub info: TrackInfo,
    pub store: Arc<TrackStore>,
    pub pyramid: WaveformPyramid,
    /// Per-channel pyramids for the split view; loaded on first use.
    pub channel_pyramids: Option<Arc<Vec<WaveformPyramid>>>,
    /// Display gain so quiet tracks are still readable (not applied to audio).
    pub display_scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub track: Option<TrackId>,
    pub start: f64,
    pub end: f64,
}

impl Selection {
    pub fn normalized(a: f64, b: f64, track: Option<TrackId>) -> Self {
        Self { track, start: a.min(b).max(0.0), end: a.max(b).max(0.0) }
    }

    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// What the current playback is doing, so seeks can resume it in kind.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PlayMode {
    ToEnd,
    Range { end: f64, looping: Option<f64> },
}

pub struct Session {
    pub media_path: PathBuf,
    pub info: MediaInfo,
    pub tracks: Vec<LoadedTrack>,
    pub project: Project,
    project_path: PathBuf,
    pub cache: CacheEntry,
    pub engine: Option<PlaybackEngine>,
    pub audio_error: Option<String>,
    pub view: TimelineView,
    playhead: f64,
    play_mode: Option<PlayMode>,
    pub selection: Option<Selection>,
    pub selected_region: Option<RegionId>,
    /// "Po" (true) or "Przed" (false).
    pub after: bool,
    undo: Vec<Project>,
    redo: Vec<Project>,
    dirty_since: Option<Instant>,
    pub save_error: Option<String>,
    channel_pyramids_rx: Option<crossbeam_channel::Receiver<(usize, Vec<WaveformPyramid>)>>,
    /// Tracks waiting for automatic detection (after "Popraw wdechy" is
    /// switched on). Drained by the app when no job is running.
    pub pending_detection: Vec<TrackId>,
    /// Application settings that affect the mix, mirrored from `Settings`
    /// by [`Session::apply_settings`].
    gain_db: f32,
    threshold: f32,
}

impl Session {
    pub fn new(imported: ImportedMedia) -> Self {
        let media_path = imported.info.path.clone();
        let project_path = Project::path_for(&media_path);
        let (project, save_error) = if project_path.exists() {
            match Project::load(&project_path) {
                Ok(p) => (p, None),
                Err(e) => {
                    log::error!("cannot load project {}: {e}", project_path.display());
                    // Never overwrite a project we could not read.
                    let backup = project_path.with_extension("json.unreadable");
                    let _ = std::fs::rename(&project_path, &backup);
                    (
                        Project::new(&media_path),
                        Some(format!("Nie udało się wczytać projektu ({e}); zachowano kopię {}", backup.display())),
                    )
                }
            }
        } else {
            (Project::new(&media_path), None)
        };

        let tracks: Vec<LoadedTrack> = imported
            .tracks
            .into_iter()
            .map(|t| {
                let display_scale = display_scale_for(&t.pyramid);
                LoadedTrack {
                    info: t.info,
                    store: Arc::new(t.store),
                    pyramid: t.pyramid,
                    channel_pyramids: None,
                    display_scale,
                }
            })
            .collect();

        let (engine, audio_error) = match PlaybackEngine::new(tracks.iter().map(|t| Arc::clone(&t.store)).collect()) {
            Ok(e) => (Some(e), None),
            Err(e) => {
                log::error!("audio unavailable: {e}");
                (None, Some(e.to_string()))
            }
        };

        let mut s = Self {
            view: TimelineView::new(imported.info.duration_secs),
            media_path,
            info: imported.info,
            tracks,
            project,
            project_path,
            cache: imported.cache,
            engine,
            audio_error,
            playhead: 0.0,
            play_mode: None,
            selection: None,
            selected_region: None,
            after: true,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty_since: None,
            save_error,
            channel_pyramids_rx: None,
            pending_detection: Vec::new(),
            gain_db: oddech_core::gain::DEFAULT_GAIN_DB,
            threshold: oddech_core::project::DEFAULT_THRESHOLD,
        };
        s.refresh_mix();
        s
    }

    pub fn project_path(&self) -> &std::path::Path {
        &self.project_path
    }

    pub fn file_name(&self) -> String {
        self.media_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn duration(&self) -> f64 {
        self.info.duration_secs
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.info.id == id)
    }

    // ---------------------------------------------------------------- playback

    pub fn is_playing(&self) -> bool {
        self.play_mode.is_some() && self.engine.as_ref().is_some_and(PlaybackEngine::is_playing)
    }

    pub fn playhead(&self) -> f64 {
        match (&self.engine, self.play_mode) {
            (Some(e), Some(_)) => e.position(),
            _ => self.playhead,
        }
    }

    /// Starts loading per-channel pyramids (split-channel view) once.
    pub fn request_channel_pyramids(&mut self) {
        if self.channel_pyramids_rx.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let jobs: Vec<(usize, TrackId, Arc<TrackStore>)> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.info.channels > 1)
            .map(|(i, t)| (i, t.info.id, Arc::clone(&t.store)))
            .collect();
        let cache = self.cache.clone();
        std::thread::spawn(move || {
            for (i, id, store) in jobs {
                match oddech_core::cache::channel_pyramids(&cache, id, &store) {
                    Ok(p) => {
                        let _ = tx.send((i, p));
                    }
                    Err(e) => log::error!("channel pyramids for track {id}: {e}"),
                }
            }
        });
        self.channel_pyramids_rx = Some(rx);
    }

    /// Call once per frame: notices when playback reached its end.
    pub fn tick(&mut self) {
        if let Some(rx) = &self.channel_pyramids_rx {
            while let Ok((i, p)) = rx.try_recv() {
                self.tracks[i].channel_pyramids = Some(Arc::new(p));
            }
        }
        if self.play_mode.is_some() && !self.engine.as_ref().is_some_and(PlaybackEngine::is_playing) {
            if let Some(e) = &self.engine {
                self.playhead = e.position();
            }
            self.play_mode = None;
        }
        self.autosave(false);
    }

    fn start(&mut self, start: f64, mode: PlayMode) {
        let Some(engine) = &self.engine else { return };
        let (end, loop_start) = match mode {
            PlayMode::ToEnd => (None, None),
            PlayMode::Range { end, looping } => (Some(end), looping),
        };
        engine.play(PlayRequest { start, end, loop_start });
        self.play_mode = Some(mode);
    }

    pub fn stop(&mut self) {
        if let Some(e) = &self.engine {
            if self.play_mode.is_some() {
                self.playhead = e.position();
            }
            e.stop();
        }
        self.play_mode = None;
    }

    /// Space: play/pause from the playhead. With "play only breath" and a
    /// selected region, plays just that region.
    pub fn toggle_play(&mut self, settings: &Settings) {
        if self.is_playing() {
            self.stop();
            return;
        }
        if settings.play_only_breath
            && let Some(r) = self.selected_region.and_then(|id| self.project.region(id)).cloned()
        {
            self.start(r.start, PlayMode::Range { end: r.end, looping: None });
            return;
        }
        let at = if self.playhead >= self.duration() - 1e-3 { 0.0 } else { self.playhead };
        self.start(at, PlayMode::ToEnd);
    }

    /// Enter: plays the selection (looping if enabled).
    pub fn play_selection(&mut self, looping: bool) {
        if let Some(sel) = self.selection.filter(|s| s.duration() > 1e-4) {
            self.start(sel.start, PlayMode::Range { end: sel.end, looping: looping.then_some(sel.start) });
        }
    }

    /// Moves the playhead. Playback continues from the new position.
    pub fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration());
        self.playhead = t;
        match self.play_mode {
            Some(PlayMode::ToEnd) => self.start(t, PlayMode::ToEnd),
            // Seeking out of a range ends range playback, but keeps playing.
            Some(PlayMode::Range { end, looping }) => {
                let inside = looping.is_some_and(|l| t >= l && t < end) || (looping.is_none() && t < end);
                let mode = if inside { PlayMode::Range { end, looping } } else { PlayMode::ToEnd };
                self.start(t, mode);
            }
            None => {
                if let Some(e) = &self.engine {
                    e.set_position(t);
                }
            }
        }
    }

    /// Pushes current mute/solo/attenuation state to the audio engine.
    pub fn refresh_mix(&mut self) {
        let Some(engine) = &self.engine else { return };
        let any_solo = self.tracks.iter().any(|t| self.project.track(t.info.id).solo);
        let tracks = self
            .tracks
            .iter()
            .map(|t| {
                let ts = self.project.track(t.info.id);
                TrackMix {
                    audible: !ts.mute && (!any_solo || ts.solo),
                    envelope: ts.fix.then(|| Arc::new(self.envelope_for(t.info.id, t.info.sample_rate))),
                }
            })
            .collect();
        engine.set_mix(MixSettings { tracks, apply_attenuation: self.after });
    }

    /// The attenuation envelope for a track at its native rate.
    pub fn envelope_for(&self, id: TrackId, rate: u32) -> GainEnvelope {
        let ts = self.project.track(id);
        let duck = oddech_core::DuckParams { gain_db: self.gain_db, fade_ms: ts.duck.fade_ms };
        GainEnvelope::build(self.project.regions.iter().filter(|r| r.track == id), &duck, &self.gate(), rate)
    }

    pub fn set_after(&mut self, after: bool) {
        self.after = after;
        self.refresh_mix();
    }

    // ------------------------------------------------------------ project edits

    /// Applies an undoable change to the project.
    pub fn edit(&mut self, f: impl FnOnce(&mut Project)) {
        let before = self.project.clone();
        f(&mut self.project);
        if self.project != before {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.after_change();
        }
    }

    /// A change that is not worth an undo step (e.g. mute/solo).
    pub fn edit_silently(&mut self, f: impl FnOnce(&mut Project)) {
        f(&mut self.project);
        self.after_change();
    }

    /// Starts a continuous edit (e.g. dragging a region edge): records the
    /// undo snapshot once; follow with `edit_silently` calls.
    pub fn begin_gesture(&mut self) {
        self.undo.push(self.project.clone());
        self.redo.clear();
    }

    fn after_change(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
        if let Some(id) = self.selected_region
            && self.project.region(id).is_none()
        {
            self.selected_region = None;
        }
        self.refresh_mix();
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.project, prev));
            self.after_change();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.project, next));
            self.after_change();
        }
    }

    /// Saves the project if it changed and settled (or immediately if `force`).
    /// "Save all": saves the project and refreshes the read-only reference
    /// copy of the labels next to the media file.
    pub fn save_all(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
        self.autosave(true);
        if self.save_error.is_some() {
            return;
        }
        let path = Project::reference_path_for(&self.media_path);
        if let Err(e) = self.project.save_read_only(&path) {
            log::error!("saving reference {} failed: {e}", path.display());
            self.save_error = Some(format!("Nie udało się zapisać kopii referencyjnej: {e}"));
        }
    }

    pub fn autosave(&mut self, force: bool) {
        let Some(since) = self.dirty_since else { return };
        if !force && since.elapsed() < AUTOSAVE_DELAY {
            return;
        }
        match self.project.save(&self.project_path) {
            Ok(()) => {
                self.dirty_since = None;
                self.save_error = None;
            }
            Err(e) => {
                log::error!("saving project failed: {e}");
                self.save_error = Some(format!("Nie udało się zapisać projektu: {e}"));
                // Retry later rather than every frame.
                self.dirty_since = Some(Instant::now());
            }
        }
    }

    // ----------------------------------------------------------------- regions

    /// Breath regions that are currently attenuated on fixed tracks, in
    /// timeline order. This is the list the review bar walks through.
    pub fn review_list(&self) -> Vec<RegionId> {
        let mut v: Vec<&Region> = self
            .project
            .regions
            .iter()
            .filter(|r| self.project.track(r.track).fix && r.is_active_breath(&self.gate()))
            .collect();
        v.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.track.cmp(&b.track)));
        v.into_iter().map(|r| r.id).collect()
    }

    /// Selects a region and moves the playhead to its start minus pre-roll.
    /// Playback, if running, continues from there.
    pub fn go_to_region(&mut self, id: RegionId, settings: &Settings) {
        let Some(r) = self.project.region(id).cloned() else { return };
        self.selected_region = Some(id);
        self.selection = None;
        self.view.reveal(r.start - settings.preroll_secs.min(1.0), r.end + 0.5);
        if settings.play_only_breath && self.is_playing() {
            self.playhead = r.start;
            self.start(r.start, PlayMode::Range { end: r.end, looping: None });
        } else {
            self.seek((r.start - settings.preroll_secs).max(0.0));
        }
    }

    /// Next/previous region in the review list relative to the selection
    /// (or to the playhead when nothing is selected).
    pub fn step_region(&mut self, forward: bool, settings: &Settings) {
        let list = self.review_list();
        if list.is_empty() {
            return;
        }
        let target = match self.selected_region.and_then(|id| list.iter().position(|&x| x == id)) {
            Some(i) if forward => list.get(i + 1).copied(),
            Some(i) => i.checked_sub(1).map(|j| list[j]),
            None => {
                let t = self.playhead();
                let starts = |id: &RegionId| self.project.region(*id).map_or(0.0, |r| r.start);
                if forward {
                    list.iter().find(|id| starts(id) > t + 1e-6).copied()
                } else {
                    list.iter().rev().find(|id| starts(id) < t - 1e-6).copied()
                }
            }
        };
        if let Some(id) = target {
            self.go_to_region(id, settings);
        }
    }

    /// "Zatwierdź": confirms the breath and moves on.
    pub fn accept_selected(&mut self, settings: &Settings) {
        let Some(id) = self.selected_region else { return };
        self.edit(|p| {
            if let Some(r) = p.region_mut(id) {
                r.kind = RegionKind::Breath;
                r.source = RegionSource::User;
            }
        });
        self.step_region(true, settings);
    }

    /// Confirms every breath currently detected (above the threshold) at
    /// once. Undoable. Returns how many were confirmed.
    pub fn accept_all(&mut self) -> usize {
        let ids: Vec<RegionId> = self
            .review_list()
            .into_iter()
            .filter(|id| self.project.region(*id).is_some_and(Region::is_pending))
            .collect();
        let n = ids.len();
        self.edit(|p| {
            for id in &ids {
                if let Some(r) = p.region_mut(*id) {
                    r.source = RegionSource::User;
                }
            }
        });
        n
    }

    /// "To nie wdech": rejects the region (kept as a negative example).
    pub fn reject_selected(&mut self, settings: &Settings) {
        let Some(id) = self.selected_region else { return };
        // Find the successor before the region leaves the review list.
        let list = self.review_list();
        let next = list.iter().position(|&x| x == id).and_then(|i| list.get(i + 1).copied());
        self.edit(|p| {
            if let Some(r) = p.region_mut(id) {
                r.kind = RegionKind::NotBreath;
                r.source = RegionSource::User;
            }
        });
        self.selected_region = None;
        if let Some(n) = next {
            self.go_to_region(n, settings);
        }
    }

    pub fn delete_selected_region(&mut self) {
        if let Some(id) = self.selected_region.take() {
            self.edit(|p| {
                p.remove_region(id);
            });
        }
    }

    /// Turns the current selection into a user breath region.
    pub fn add_region_from_selection(&mut self) -> Option<RegionId> {
        let sel = self.selection.filter(|s| s.duration() >= 0.01)?;
        let track = sel.track.or_else(|| {
            self.tracks.iter().find(|t| self.project.track(t.info.id).fix).map(|t| t.info.id)
        })?;
        let mut new_id = None;
        self.edit(|p| {
            let id = p.allocate_region_id();
            p.regions.push(Region {
                id,
                track,
                start: sel.start,
                end: sel.end,
                kind: RegionKind::Breath,
                source: RegionSource::User,
                confidence: None,
                gain_db: None,
                fade_ms: None,
                silence_db: Vec::new(),
            });
            p.track_mut(track).fix = true;
            new_id = Some(id);
        });
        self.selection = None;
        self.selected_region = new_id;
        new_id
    }

    /// Switches "Popraw wdechy" for a track. Switching on a track that was
    /// never analysed queues detection for it.
    pub fn set_fix(&mut self, id: TrackId, fix: bool) {
        self.edit(|p| p.track_mut(id).fix = fix);
        let analysed = self.project.regions.iter().any(|r| r.track == id && r.source == RegionSource::Model)
            || self.project.track(id).detected;
        if fix && !analysed && !self.pending_detection.contains(&id) {
            self.pending_detection.push(id);
        }
    }

    /// Replaces the model's regions on the analysed tracks with fresh
    /// detections. The user's regions (confirmed, rejected or drawn) are
    /// kept, and new detections that overlap them are dropped: user
    /// decisions always win.
    pub fn apply_detections(&mut self, results: Vec<crate::detection::TrackResult>) -> usize {
        let mut added = 0;
        self.edit(|p| {
            for result in results {
                p.regions.retain(|r| !(r.track == result.id && r.source == RegionSource::Model));
                let user: Vec<(f64, f64)> = p
                    .regions
                    .iter()
                    .filter(|r| r.track == result.id)
                    .map(|r| (r.start, r.end))
                    .collect();
                for d in result.proposals {
                    let overlaps_user = user.iter().any(|&(s, e)| {
                        let inter = (e.min(d.end) - s.max(d.start)).max(0.0);
                        inter > 0.5 * (d.end - d.start).min(e - s)
                    });
                    if overlaps_user {
                        continue;
                    }
                    let id = p.allocate_region_id();
                    p.regions.push(Region {
                        id,
                        track: result.id,
                        start: d.start,
                        end: d.end,
                        kind: RegionKind::Breath,
                        source: RegionSource::Model,
                        confidence: Some(d.confidence),
                        gain_db: None,
                        fade_ms: None,
                        silence_db: d.silence_db,
                    });
                    added += 1;
                }
                p.track_mut(result.id).detected = true;
            }
        });
        added
    }

    /// Breath attenuation in dB (application setting).
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// What decides whether model regions are active.
    pub fn gate(&self) -> oddech_core::Gate {
        self.project.gate(self.threshold)
    }

    /// Takes the application-wide gain and sensitivity; call every frame.
    pub fn apply_settings(&mut self, settings: &Settings) {
        let same_gain = self.gain_db == settings.breath_gain_db
            || (self.gain_db.is_infinite() && settings.breath_gain_db.is_infinite());
        if !same_gain || self.threshold != settings.threshold {
            self.gain_db = settings.breath_gain_db;
            self.threshold = settings.threshold;
            self.refresh_mix();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.autosave(true);
    }
}

/// Chooses a display gain from the 99.5th percentile of block peaks, so a
/// single click does not flatten the rest of the waveform.
fn display_scale_for(pyramid: &WaveformPyramid) -> f32 {
    let len = pyramid.len_samples() as f64;
    if len == 0.0 {
        return 1.0;
    }
    // ~4096 columns across the whole file.
    let cols = 4096;
    let spp = (len / cols as f64).max(oddech_core::pyramid::BASE_BUCKET as f64);
    let mut out = Vec::new();
    pyramid.columns(0.0, spp, cols, &mut out);
    let mut peaks: Vec<f32> = out.iter().filter(|m| !m.is_empty()).map(|m| m.max.max(-m.min)).collect();
    if peaks.is_empty() {
        return 1.0;
    }
    peaks.sort_by(f32::total_cmp);
    let p = peaks[((peaks.len() - 1) as f64 * 0.995) as usize];
    if p < 1e-4 { 1.0 } else { (0.9 / p).clamp(1.0, 30.0) }
}
