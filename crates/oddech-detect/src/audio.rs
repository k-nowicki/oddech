//! Audio preparation shared by the models.

use oddech_core::resample::SincInterpolator;
use oddech_core::store::TrackStore;

pub const RATE_16K: u32 = 16_000;

/// Converts a track's mono mix to 16 kHz, the rate both models expect.
pub fn mono_16k(store: &TrackStore) -> Vec<f32> {
    let interp = SincInterpolator::new(store.rate(), RATE_16K);
    let n_out = (store.len() as f64 / interp.step()).floor() as usize;
    const BLOCK: usize = 1 << 16;
    let mut out = vec![0.0f32; n_out];
    let mut src = Vec::new();
    for (b, chunk) in out.chunks_mut(BLOCK).enumerate() {
        let pos = (b * BLOCK) as f64 * interp.step();
        let (s, e) = interp.source_window(pos, chunk.len());
        src.resize((e - s) as usize, 0.0);
        store.read_mix(s, &mut src);
        interp.process(&src, s, pos, chunk);
    }
    out
}
