//! Running breath detection for a session's tracks (background job body).
//!
//! Detection uses Respiro-en (see `oddech_detect::respiro`). Each proposal
//! carries a loudness profile of its surroundings so that the
//! "Dyskryminacja" sliders work without re-analysis.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use oddech_core::TrackId;
use oddech_core::region::Discrimination;
use oddech_core::store::TrackStore;
use oddech_detect::audio::mono_16k;
use oddech_detect::loudness;
use oddech_detect::respiro::{self, Respiro, RespiroError};

use crate::jobs::JobStatus;

pub struct TrackInput {
    pub id: TrackId,
    pub name: String,
    pub store: Arc<TrackStore>,
}

/// A detected breath before it becomes a region.
pub struct Proposal {
    pub start: f64,
    pub end: f64,
    pub confidence: f32,
    pub silence_db: Vec<u8>,
}

pub struct TrackResult {
    pub id: TrackId,
    pub proposals: Vec<Proposal>,
}

/// Name of the detector, shown with quality metrics.
pub const DETECTOR_NAME: &str = "Respiro-en";

/// Detects breaths on each track.
pub fn run(tracks: &[TrackInput], status: &JobStatus) -> Result<Vec<TrackResult>, String> {
    let cancelled = || status.progress.cancel.load(Ordering::Relaxed);
    status.set_label("Wczytywanie modelu wykrywania wdechów…");
    let model = Respiro::shared().map_err(|e| e.to_string())?;

    let n = tracks.len() as f64;
    let mut results = Vec::with_capacity(tracks.len());
    for (i, t) in tracks.iter().enumerate() {
        if cancelled() {
            return Err("przerwano".into());
        }
        status.set_label(format!("Wykrywanie wdechów: {}", t.name));
        let base = i as f64 / n;
        status.progress.set(base);
        let audio = mono_16k(&t.store);
        let probs = model
            .probabilities(&audio, &|p| status.progress.set(base + f64::from(p) * 0.95 / n), &cancelled)
            .map_err(|e| match e {
                RespiroError::Cancelled => "przerwano".to_owned(),
                e => e.to_string(),
            })?;
        let db = loudness::frame_db(&audio);
        let speech = loudness::speech_level(&db);
        let all = respiro::segments(&probs);
        let proposals: Vec<Proposal> = all
            .iter()
            .map(|&(start, end, confidence)| Proposal {
                start,
                end,
                confidence,
                silence_db: loudness::silence_profile(&db, speech, start, end),
            })
            // Beyond what the sliders can reach: never shown, not stored.
            .filter(|p| p.silence_db.last().is_some_and(|&d| f32::from(d) <= Discrimination::MAX_DROP_DB))
            .collect();
        log::info!(
            "{}: {} breaths detected, {} stored (speech level {speech:.1} dBFS, on {})",
            t.name,
            all.len(),
            proposals.len(),
            model.device()
        );
        results.push(TrackResult { id: t.id, proposals });
    }
    status.progress.set(1.0);
    Ok(results)
}
