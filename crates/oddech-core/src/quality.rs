//! Detection quality measured against the user's review decisions.
//!
//! Ground truth comes from reviewed regions only:
//! * a model region the user confirmed is a breath (positive);
//! * a model region the user rejected ("to nie wdech") is not (negative);
//! * a region the user drew themselves is a breath the model missed.
//!
//! A model region counts as *predicted* when its confidence reaches the
//! threshold. Unreviewed model regions are ignored.

use crate::region::{Region, RegionKind, RegionSource};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Quality {
    pub true_pos: usize,
    pub false_pos: usize,
    pub false_neg: usize,
    pub true_neg: usize,
}

impl Quality {
    pub fn precision(&self) -> Option<f32> {
        let d = self.true_pos + self.false_pos;
        (d > 0).then(|| self.true_pos as f32 / d as f32)
    }

    pub fn recall(&self) -> Option<f32> {
        let d = self.true_pos + self.false_neg;
        (d > 0).then(|| self.true_pos as f32 / d as f32)
    }

    pub fn f1(&self) -> Option<f32> {
        let (p, r) = (self.precision()?, self.recall()?);
        (p + r > 0.0).then(|| 2.0 * p * r / (p + r))
    }

    pub fn labelled(&self) -> usize {
        self.true_pos + self.false_pos + self.false_neg + self.true_neg
    }
}

pub fn evaluate<'a>(regions: impl IntoIterator<Item = &'a Region>, threshold: f32) -> Quality {
    let mut q = Quality::default();
    for r in regions {
        if r.source != RegionSource::User {
            continue; // not reviewed
        }
        let is_breath = r.kind == RegionKind::Breath;
        match r.confidence {
            // Drawn by the user: the model never proposed it.
            None => {
                if is_breath {
                    q.false_neg += 1;
                }
            }
            Some(c) => match (c >= threshold, is_breath) {
                (true, true) => q.true_pos += 1,
                (true, false) => q.false_pos += 1,
                (false, true) => q.false_neg += 1,
                (false, false) => q.true_neg += 1,
            },
        }
    }
    q
}

/// Threshold (in steps of 0.05) that maximizes F1 on the reviewed regions.
pub fn best_threshold(regions: &[Region]) -> Option<(f32, Quality)> {
    (1..20)
        .map(|i| i as f32 * 0.05)
        .map(|t| (t, evaluate(regions, t)))
        .filter(|(_, q)| q.f1().is_some())
        .max_by(|a, b| a.1.f1().unwrap_or(0.0).total_cmp(&b.1.f1().unwrap_or(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::RegionId;
    use crate::track::TrackId;

    fn r(kind: RegionKind, source: RegionSource, confidence: Option<f32>) -> Region {
        Region {
            id: RegionId(0),
            track: TrackId(0),
            start: 0.0,
            end: 1.0,
            kind,
            source,
            confidence,
            gain_db: None,
            fade_ms: None,
            silence_db: Vec::new(),
        }
    }

    #[test]
    fn counts_and_scores() {
        use RegionKind::*;
        use RegionSource::*;
        let regions = vec![
            r(Breath, User, Some(0.9)),    // TP
            r(Breath, User, Some(0.8)),    // TP
            r(NotBreath, User, Some(0.7)), // FP
            r(Breath, User, None),         // FN (drawn)
            r(Breath, User, Some(0.2)),    // FN (below threshold)
            r(NotBreath, User, Some(0.1)), // TN
            r(Breath, Model, Some(0.9)),   // unreviewed: ignored
        ];
        let q = evaluate(&regions, 0.5);
        assert_eq!(q, Quality { true_pos: 2, false_pos: 1, false_neg: 2, true_neg: 1 });
        assert_eq!(q.precision(), Some(2.0 / 3.0));
        assert_eq!(q.recall(), Some(0.5));
        let (t, best) = best_threshold(&regions).unwrap();
        assert!(best.f1().unwrap() >= q.f1().unwrap());
        assert!(t <= 0.2 + 1e-6, "lowering the threshold catches the 0.2 breath: {t}");
    }
}
