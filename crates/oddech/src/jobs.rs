//! Long-running work (export, detection) on background threads, with
//! progress and cancellation visible in the UI.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, bounded};
use oddech_core::TrackId;
use oddech_core::gain::GainEnvelope;
use oddech_core::interchange::{self, FcpxmlMedia, Label, Marker};
use oddech_core::store::TrackStore;
use oddech_media::ImportProgress;
use oddech_media::export::{self, ExportTrack, WavSampleFormat};

use crate::session::Session;
use crate::settings::{ExportFormat, Settings, WavDepth};
use crate::ui::options::LabelFormat;

pub enum JobOutcome {
    Exported(Vec<PathBuf>),
    Detected(Vec<crate::detection::TrackResult>),
    Failed(String),
    Cancelled,
}

/// Progress and a human-readable stage label, shared with the worker.
#[derive(Default)]
pub struct JobStatus {
    pub progress: Arc<ImportProgress>,
    label: Mutex<String>,
}

impl JobStatus {
    pub fn set_label(&self, label: impl Into<String>) {
        *self.label.lock().unwrap_or_else(|e| e.into_inner()) = label.into();
    }

    pub fn label(&self) -> String {
        self.label.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub struct Job {
    pub title: String,
    pub status: Arc<JobStatus>,
    rx: Receiver<JobOutcome>,
}

impl Job {
    fn spawn(
        title: &str,
        ctx: &egui::Context,
        work: impl FnOnce(&JobStatus) -> JobOutcome + Send + 'static,
    ) -> Result<Self, String> {
        let status = Arc::new(JobStatus::default());
        let (tx, rx) = bounded(1);
        let (st, ctx) = (Arc::clone(&status), ctx.clone());
        std::thread::Builder::new()
            .name(format!("oddech-job-{title}"))
            // Model runtimes (whisper.cpp in particular) need more than the
            // default 2 MiB of stack.
            .stack_size(64 << 20)
            .spawn(move || {
                let outcome = work(&st);
                let _ = tx.send(outcome);
                ctx.request_repaint();
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { title: title.to_owned(), status, rx })
    }

    pub fn poll(&self) -> Option<JobOutcome> {
        self.rx.try_recv().ok()
    }

    pub fn cancel(&self) {
        self.status.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Text for the status line.
    pub fn describe(&self) -> (String, f32) {
        let label = self.status.label();
        (if label.is_empty() { self.title.clone() } else { label }, self.status.progress.fraction() as f32)
    }
}

struct Snapshot {
    id: TrackId,
    name: String,
    store: Arc<TrackStore>,
    envelope: GainEnvelope,
}

/// Processed state of every fixed track, captured so the export is not
/// affected by edits made while it runs.
fn fixed_tracks(session: &Session) -> Vec<Snapshot> {
    session
        .tracks
        .iter()
        .filter(|t| session.project.track(t.info.id).fix)
        .map(|t| Snapshot {
            id: t.info.id,
            name: t.info.display_name(),
            store: Arc::clone(&t.store),
            envelope: session.envelope_for(t.info.id, t.info.sample_rate),
        })
        .collect()
}

fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    s.trim_matches('_').chars().take(40).collect()
}

/// Starts exporting in the format chosen in settings to `target`: the
/// video file, or the folder for per-track WAVs.
pub fn start_export(session: &Session, settings: &Settings, target: PathBuf, ctx: &egui::Context) -> Result<Job, String> {
    let tracks = fixed_tracks(session);
    if tracks.is_empty() {
        return Err("Włącz „Popraw wdechy” na ścieżce, którą chcesz wyeksportować.".into());
    }
    let media = session.media_path.clone();
    let format = settings.export_format;
    let depth = match settings.wav_depth {
        WavDepth::Float32 => WavSampleFormat::Float32,
        WavDepth::Int24 => WavSampleFormat::Int24,
    };
    let title = match format {
        ExportFormat::Video => "Eksport wideo",
        ExportFormat::WavTracks => "Eksport ścieżek WAV",
    };
    Job::spawn(title, ctx, move |status| run_export(&media, &target, &tracks, format, depth, &status.progress))
}

/// Starts breath detection on `ids` (fixed tracks).
pub fn start_detection(session: &Session, ids: &[TrackId], ctx: &egui::Context) -> Result<Job, String> {
    let inputs: Vec<crate::detection::TrackInput> = session
        .tracks
        .iter()
        .filter(|t| ids.contains(&t.info.id))
        .map(|t| crate::detection::TrackInput { id: t.info.id, name: t.info.display_name(), store: Arc::clone(&t.store) })
        .collect();
    if inputs.is_empty() {
        return Err("Włącz „Popraw wdechy” na ścieżce, na której chcesz wykryć wdechy.".into());
    }
    Job::spawn("Wykrywanie wdechów", ctx, move |status| {
        match crate::detection::run(&inputs, status) {
            Ok(results) => JobOutcome::Detected(results),
            Err(_) if status.progress.cancel.load(std::sync::atomic::Ordering::Relaxed) => JobOutcome::Cancelled,
            Err(e) => JobOutcome::Failed(e),
        }
    })
}

/// `<dir>/<base>.<ext>`, or `<base> (2).<ext>`… when that exists.
fn unique_in(dir: &Path, base: &str, ext: &str) -> PathBuf {
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{base}.{ext}") } else { format!("{base} ({n}).{ext}") }))
        .find(|p| !p.exists())
        .expect("some name is free")
}

fn run_export(
    media: &Path,
    target: &Path,
    tracks: &[Snapshot],
    format: ExportFormat,
    depth: WavSampleFormat,
    progress: &ImportProgress,
) -> JobOutcome {
    let export_tracks: Vec<ExportTrack<'_>> =
        tracks.iter().map(|t| ExportTrack { id: t.id, store: &t.store, envelope: &t.envelope }).collect();
    let result = match format {
        ExportFormat::Video => export::remux(media, target, &export_tracks, progress).map(|()| vec![target.to_owned()]),
        ExportFormat::WavTracks => {
            let mut outs = Vec::new();
            let mut res = Ok(());
            let stem = media.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            for (t, et) in tracks.iter().zip(&export_tracks) {
                let out = unique_in(target, &format!("{stem}-oddech-{}", slug(&t.name)), "wav");
                // Per-track progress is shown as overall progress.
                res = export::export_wav(et, &out, depth, progress);
                if res.is_err() {
                    break;
                }
                outs.push(out);
            }
            res.map(|()| outs)
        }
    };
    match result {
        Ok(paths) => JobOutcome::Exported(paths),
        Err(oddech_media::MediaError::Cancelled) => JobOutcome::Cancelled,
        Err(e) => JobOutcome::Failed(e.to_string()),
    }
}

fn markers(session: &Session) -> Vec<Marker> {
    session
        .review_list()
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let r = session.project.region(*id)?;
            let track = session.track_index(r.track).map(|i| session.tracks[i].info.display_name()).unwrap_or_default();
            let gain_db = r.gain_db.unwrap_or(session.gain_db());
            Some(Marker { start: r.start, end: r.end, name: format!("Wdech {}", i + 1), note: track, gain_db })
        })
        .collect()
}

/// Writes the recording cut at breaths (FCPXML) to `out`.
pub fn export_timeline(session: &Session, start_hours: u8, out: &Path) -> Result<(), String> {
    let markers = markers(session);
    let fps = session.info.video_fps.unwrap_or(25.0);
    let name = session.file_name();
    let text = {
        {
            let first_audio = session.tracks.first().map(|t| &t.info);
            let (w, h) = session.info.video_size.unwrap_or((1920, 1080));
            let abs = std::fs::canonicalize(&session.media_path).unwrap_or_else(|_| session.media_path.clone());
            let media = FcpxmlMedia {
                name: &name,
                path: &abs,
                duration: session.duration(),
                fps,
                width: w,
                height: h,
                audio_channels: first_audio.map_or(2, |t| t.channels),
                audio_sources: session.tracks.len(),
                audio_rate: first_audio.map_or(48_000, |t| t.sample_rate),
            };
            interchange::fcpxml(&media, u32::from(start_hours) * 3600, &markers)
        }
    };
    std::fs::write(out, text).map_err(|e| e.to_string())
}

/// Writes all regions (with decisions) as training labels to `out`.
pub fn export_labels(session: &Session, format: LabelFormat, out: &Path) -> Result<(), String> {
    let names: Vec<(TrackId, String)> = session.tracks.iter().map(|t| (t.info.id, t.info.display_name())).collect();
    let name_of = |id: TrackId| names.iter().find(|(i, _)| *i == id).map_or("", |(_, n)| n.as_str());
    let mut regions: Vec<_> = session.project.regions.iter().collect();
    regions.sort_by(|a, b| a.track.cmp(&b.track).then(a.start.total_cmp(&b.start)));
    let labels: Vec<Label<'_>> = regions.iter().map(|r| Label::from_region(r, name_of(r.track))).collect();
    let text = match format {
        LabelFormat::Csv => interchange::labels_csv(&labels),
        LabelFormat::Json => interchange::labels_json(&session.file_name(), &labels),
    };
    std::fs::write(out, text).map_err(|e| e.to_string())
}
