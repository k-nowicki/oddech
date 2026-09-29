//! Breath detection.
//!
//! The app detects breaths with Respiro-en ([`respiro`]) and describes how
//! far each one is from speech ([`loudness`]). Speech transcription
//! (whisper, `transcribe`) is not used for detection; it is built only with
//! the `whisper` feature.

pub mod audio;
pub mod loudness;
pub mod models;
pub mod respiro;
#[cfg(feature = "whisper")]
pub mod transcribe;
