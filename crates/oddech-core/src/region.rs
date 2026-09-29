//! Regions: the annotation layer.
//!
//! A region marks a time span on one track. Regions carry *what* a span is
//! (breath or not) and *who* said so (model or user). How loud a breath is
//! played back is a separate concern handled by [`crate::gain`]; the only
//! link is the optional per-region override stored here.

use serde::{Deserialize, Serialize};

use crate::track::TrackId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RegionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    Breath,
    NotBreath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionSource {
    /// Proposed by the detector and not yet reviewed.
    Model,
    /// Created or confirmed by the user. User regions are ground truth.
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub id: RegionId,
    pub track: TrackId,
    /// Start in seconds from the beginning of the file.
    pub start: f64,
    /// End in seconds (exclusive). Always `> start`.
    pub end: f64,
    pub kind: RegionKind,
    pub source: RegionSource,
    /// Detector confidence in `[0, 1]`. Kept after user review so that
    /// training data retains the model's original opinion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Per-region attenuation override in dB (`None` = inherit track value,
    /// `-inf` is represented as `f32::NEG_INFINITY` → full mute).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_db")]
    pub gain_db: Option<f32>,
    /// Per-region fade override in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_ms: Option<f32>,
    /// Model regions: `silence_db[i]` is how many dB below the track's
    /// speech level the loudest sound within ±(i + 1) s of the region is
    /// (0 = at speech level or louder). Drives [`Discrimination`]; empty
    /// means unknown and never discriminates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub silence_db: Vec<u8>,
}

/// Ignores model detections far from speech: a detection counts only when
/// something at most `drop_db` below the speech level sounds within
/// `distance_s` of it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Discrimination {
    pub distance_s: u8,
    pub drop_db: f32,
}

impl Discrimination {
    pub const MAX_DISTANCE_S: u8 = 30;
    pub const MAX_DROP_DB: f32 = 30.0;
}

impl Default for Discrimination {
    fn default() -> Self {
        Self { distance_s: 10, drop_db: 10.0 }
    }
}

/// Everything that decides whether a model region is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gate {
    /// Minimum confidence.
    pub threshold: f32,
    pub discrimination: Discrimination,
}

impl Region {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    pub fn overlaps(&self, start: f64, end: f64) -> bool {
        self.start < end && start < self.end
    }

    /// Whether this region should be attenuated.
    ///
    /// User decisions always win; model proposals are active only when their
    /// confidence reaches the threshold and they are near speech.
    pub fn is_active_breath(&self, gate: &Gate) -> bool {
        match (self.kind, self.source) {
            (RegionKind::NotBreath, _) => false,
            (RegionKind::Breath, RegionSource::User) => true,
            (RegionKind::Breath, RegionSource::Model) => {
                self.confidence.unwrap_or(1.0) >= gate.threshold && self.near_speech(&gate.discrimination)
            }
        }
    }

    /// False for model regions that discrimination hides (they are not
    /// shown at all, unlike low-confidence ones).
    pub fn near_speech(&self, d: &Discrimination) -> bool {
        if self.source == RegionSource::User || self.silence_db.is_empty() {
            return true;
        }
        let i = usize::from(d.distance_s.max(1)).min(self.silence_db.len()) - 1;
        f32::from(self.silence_db[i]) <= d.drop_db
    }

    /// Whether the user still has to review this region.
    pub fn is_pending(&self) -> bool {
        self.source == RegionSource::Model
    }
}

/// JSON cannot represent infinities, so full mute is stored as `null`
/// inside a present field: `"gain_db": "mute"`.
mod opt_db {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Db(f32),
        Word(String),
    }

    pub fn serialize<S: Serializer>(v: &Option<f32>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(db) if db.is_finite() => Repr::Db(*db).serialize(s),
            Some(_) => Repr::Word("mute".into()).serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
        Ok(match Option::<Repr>::deserialize(d)? {
            None => None,
            Some(Repr::Db(v)) => Some(v),
            Some(Repr::Word(w)) if w == "mute" => Some(f32::NEG_INFINITY),
            Some(Repr::Word(w)) => {
                return Err(serde::de::Error::custom(format!("invalid gain value {w:?}")));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(kind: RegionKind, source: RegionSource, confidence: Option<f32>) -> Region {
        Region {
            id: RegionId(1),
            track: TrackId(1),
            start: 1.0,
            end: 1.5,
            kind,
            source,
            confidence,
            gain_db: None,
            fade_ms: None,
            silence_db: Vec::new(),
        }
    }

    fn gate(threshold: f32) -> Gate {
        Gate { threshold, discrimination: Discrimination::default() }
    }

    #[test]
    fn discrimination_hides_model_regions_far_from_speech() {
        let mut model = region(RegionKind::Breath, RegionSource::Model, Some(0.9));
        // Loudest sound: 25 dB below speech within 1-4 s, speech level from 5 s.
        model.silence_db = vec![25, 25, 25, 25, 0, 0];
        let d = |distance_s, drop_db| Discrimination { distance_s, drop_db };
        assert!(!model.near_speech(&d(2, 10.0)));
        assert!(model.near_speech(&d(2, 25.0)));
        assert!(model.near_speech(&d(5, 10.0)));
        assert!(model.near_speech(&d(30, 10.0)), "beyond the profile the last value holds");
        assert!(!model.is_active_breath(&Gate { threshold: 0.5, discrimination: d(1, 10.0) }));
        let mut user = model.clone();
        user.source = RegionSource::User;
        assert!(user.near_speech(&d(1, 0.0)));
    }

    #[test]
    fn activity_respects_threshold_and_user_decisions() {
        let model = region(RegionKind::Breath, RegionSource::Model, Some(0.5));
        assert!(model.is_active_breath(&gate(0.4)));
        assert!(!model.is_active_breath(&gate(0.6)));
        let user = region(RegionKind::Breath, RegionSource::User, Some(0.1));
        assert!(user.is_active_breath(&gate(0.9)));
        let rejected = region(RegionKind::NotBreath, RegionSource::User, Some(0.99));
        assert!(!rejected.is_active_breath(&gate(0.0)));
    }

    #[test]
    fn mute_override_round_trips_through_json() {
        let mut r = region(RegionKind::Breath, RegionSource::User, None);
        r.gain_db = Some(f32::NEG_INFINITY);
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"mute\""));
        let back: Region = serde_json::from_str(&json).unwrap();
        assert_eq!(back.gain_db, Some(f32::NEG_INFINITY));

        r.gain_db = Some(-12.0);
        let back: Region = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.gain_db, Some(-12.0));
    }
}
