//! Core domain model and signal-processing primitives for Oddech.
//!
//! This crate is deliberately free of I/O-heavy or platform dependencies
//! (FFmpeg, audio devices, GPU): everything here is pure data and math,
//! which keeps it fast to compile and easy to test.

pub mod cache;
pub mod gain;
pub mod interchange;
pub mod project;
pub mod pyramid;
pub mod quality;
pub mod region;
pub mod render;
pub mod resample;
pub mod spectrogram;
pub mod store;
pub mod time;
pub mod track;

pub use gain::{DuckParams, GainEnvelope};
pub use region::{Discrimination, Gate, Region, RegionId, RegionKind, RegionSource};
pub use track::{TrackId, TrackInfo};
