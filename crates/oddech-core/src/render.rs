//! The one function that turns stored samples into processed samples.
//!
//! Playback and export both call [`render_source_block`], which guarantees
//! that exported files match what was heard, sample for sample, at the
//! track's native rate.

use crate::gain::GainEnvelope;
use crate::store::TrackStore;

/// Renders channel `ch` of `store` for samples `[start, start + out.len())`,
/// applying `envelope` if given. Samples outside the signal are zero.
pub fn render_source_block(
    store: &TrackStore,
    ch: usize,
    envelope: Option<&GainEnvelope>,
    start: i64,
    out: &mut [f32],
    scratch: &mut Vec<f32>,
) {
    store.read(ch, start, out);
    if let Some(env) = envelope {
        env.apply(start, out, scratch);
    }
}
