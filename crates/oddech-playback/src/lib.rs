//! Real-time playback.
//!
//! Architecture:
//!
//! ```text
//!  UI ──commands──▶ feeder thread ──[ring: samples + block markers]──▶ audio callback
//!        ◀──position (atomic, derived from consumed samples)──────────────┘
//! ```
//!
//! * [`mixer::Mixer`] renders blocks: reads each audible track, applies the
//!   breath gain envelope at the track's native rate (via the same function
//!   the exporter uses), converts to the device rate, and mixes to the
//!   device's channel layout.
//! * [`transport`] moves rendered audio to the callback through a lock-free
//!   ring buffer. Each block carries a marker with its timeline position, so
//!   the playhead is computed from samples actually handed to the device —
//!   it cannot drift.
//! * [`engine::PlaybackEngine`] owns the cpal stream and the feeder thread.

pub mod engine;
pub mod mixer;
pub mod transport;

pub use engine::{PlaybackEngine, PlaybackError};
pub use mixer::{MixSettings, PlayRequest, TrackMix};
