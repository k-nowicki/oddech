//! Lock-free hand-off of rendered audio from the feeder thread to the
//! audio callback, with sample-accurate position reporting.
//!
//! The producer pushes a block of interleaved samples and *then* a marker
//! describing where on the timeline that block starts. The consumer only
//! starts a block after popping its marker, so it always knows the exact
//! timeline frame of every sample it hands to the device. The reported
//! position is therefore a pure function of samples consumed: no clocks,
//! no drift.
//!
//! Flushing (seek/stop) uses a generation counter: the producer bumps it and
//! waits until the consumer, on its next callback, discards everything
//! queued and acknowledges.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
struct Marker {
    /// Timeline frame (device rate) of the block's first frame.
    first_frame: i64,
    frames: u32,
}

/// State shared between producer, consumer and UI.
#[derive(Debug, Default)]
pub struct TransportShared {
    flush_gen: AtomicU64,
    ack_gen: AtomicU64,
    /// Timeline frame of the next frame the device will play.
    position: AtomicI64,
    /// Total frames handed to the device from queued blocks (not silence).
    frames_played: AtomicU64,
    /// Whether the last callback played queued audio.
    audible: AtomicBool,
}

impl TransportShared {
    pub fn position_frames(&self) -> i64 {
        self.position.load(Ordering::Acquire)
    }

    pub fn set_position_frames(&self, frame: i64) {
        self.position.store(frame, Ordering::Release);
    }

    pub fn frames_played(&self) -> u64 {
        self.frames_played.load(Ordering::Relaxed)
    }

    pub fn is_audible(&self) -> bool {
        self.audible.load(Ordering::Relaxed)
    }
}

pub struct Producer {
    samples: rtrb::Producer<f32>,
    markers: rtrb::Producer<Marker>,
    channels: usize,
    shared: Arc<TransportShared>,
}

pub struct Consumer {
    samples: rtrb::Consumer<f32>,
    markers: rtrb::Consumer<Marker>,
    channels: usize,
    shared: Arc<TransportShared>,
    current: Option<(Marker, u32)>,
    seen_gen: u64,
}

/// Creates a transport holding up to `capacity_frames` frames.
pub fn transport(capacity_frames: usize, channels: usize) -> (Producer, Consumer, Arc<TransportShared>) {
    let shared = Arc::new(TransportShared::default());
    let (sp, sc) = rtrb::RingBuffer::new(capacity_frames * channels);
    // One marker per block; blocks are never smaller than a few frames.
    let (mp, mc) = rtrb::RingBuffer::new(capacity_frames.max(64));
    (
        Producer { samples: sp, markers: mp, channels, shared: Arc::clone(&shared) },
        Consumer {
            samples: sc,
            markers: mc,
            channels,
            shared: Arc::clone(&shared),
            current: None,
            seen_gen: 0,
        },
        shared,
    )
}

impl Producer {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames that can be pushed right now.
    pub fn free_frames(&self) -> usize {
        if self.markers.slots() == 0 { 0 } else { self.samples.slots() / self.channels }
    }

    /// Pushes one block. `interleaved.len()` must be a multiple of the
    /// channel count and fit in [`free_frames`](Self::free_frames).
    pub fn push_block(&mut self, first_frame: i64, interleaved: &[f32]) -> bool {
        let frames = interleaved.len() / self.channels;
        if frames == 0 || frames > self.free_frames() {
            return false;
        }
        if self.samples.push_entire_slice(interleaved).is_err() {
            return false;
        }
        self.markers.push(Marker { first_frame, frames: frames as u32 }).is_ok()
    }

    /// Discards everything queued. Blocks until the consumer acknowledges
    /// or `timeout` passes (e.g. when the device is stalled).
    pub fn flush(&mut self, timeout: Duration) {
        let generation = self.shared.flush_gen.fetch_add(1, Ordering::AcqRel) + 1;
        let deadline = Instant::now() + timeout;
        while self.shared.ack_gen.load(Ordering::Acquire) < generation {
            if Instant::now() >= deadline {
                log::warn!("audio flush not acknowledged within {timeout:?}");
                return;
            }
            std::thread::sleep(Duration::from_micros(500));
        }
    }
}

impl Consumer {
    /// Fills `out` (interleaved) from the queue; underruns become silence.
    /// Real-time safe: no locks, no allocation.
    pub fn fill(&mut self, out: &mut [f32]) {
        let generation = self.shared.flush_gen.load(Ordering::Acquire);
        if generation != self.seen_gen {
            self.discard_all();
            self.seen_gen = generation;
            self.shared.ack_gen.store(generation, Ordering::Release);
        }

        let mut written = 0;
        let total_frames = out.len() / self.channels;
        let mut played = 0u64;
        while written < total_frames {
            let (marker, done) = match self.current {
                Some((m, done)) if done < m.frames => (m, done),
                _ => match self.markers.pop() {
                    Ok(m) => {
                        self.current = Some((m, 0));
                        (m, 0)
                    }
                    Err(_) => break,
                },
            };
            let take = ((marker.frames - done) as usize).min(total_frames - written);
            let dst = &mut out[written * self.channels..(written + take) * self.channels];
            if self.samples.pop_entire_slice(dst).is_err() {
                // Cannot happen: samples are pushed before their marker.
                dst.fill(0.0);
            }
            let done = done + take as u32;
            self.current = Some((marker, done));
            written += take;
            played += take as u64;
            self.shared.position.store(marker.first_frame + i64::from(done), Ordering::Release);
        }
        out[written * self.channels..].fill(0.0);
        self.shared.frames_played.fetch_add(played, Ordering::Relaxed);
        self.shared.audible.store(played > 0, Ordering::Relaxed);
    }

    fn discard_all(&mut self) {
        self.current = None;
        while self.markers.pop().is_ok() {}
        let n = self.samples.slots();
        if let Ok(chunk) = self.samples.read_chunk(n) {
            chunk.commit_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_tracks_consumed_frames_exactly() {
        let (mut p, mut c, shared) = transport(1024, 2);
        let block: Vec<f32> = (0..200).map(|i| i as f32).collect(); // 100 frames
        assert!(p.push_block(5000, &block));
        let mut out = vec![9.0; 60]; // 30 frames
        c.fill(&mut out);
        assert_eq!(shared.position_frames(), 5030);
        assert_eq!(&out[..4], &[0.0, 1.0, 2.0, 3.0]);
        let mut out = vec![9.0; 200]; // 100 frames: 70 available, 30 silence
        c.fill(&mut out);
        assert_eq!(shared.position_frames(), 5100);
        assert_eq!(shared.frames_played(), 100);
        assert!(out[140..].iter().all(|&x| x == 0.0));
    }

    #[test]
    fn position_follows_discontinuous_blocks() {
        let (mut p, mut c, shared) = transport(1024, 1);
        p.push_block(100, &[1.0; 10]);
        p.push_block(0, &[2.0; 10]); // loop wrap
        let mut out = vec![0.0; 15];
        c.fill(&mut out);
        assert_eq!(shared.position_frames(), 5);
        assert_eq!(&out[9..11], &[1.0, 2.0]);
    }

    #[test]
    fn flush_discards_queued_audio() {
        let (mut p, mut c, shared) = transport(1024, 1);
        p.push_block(0, &[1.0; 500]);
        let handle = std::thread::spawn(move || {
            p.flush(Duration::from_secs(5));
            p
        });
        // Simulate the audio callback running periodically.
        let mut out = vec![0.0; 4];
        while !handle.is_finished() {
            c.fill(&mut out);
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut p = handle.join().unwrap();
        p.push_block(9000, &[2.0; 10]);
        c.fill(&mut out);
        assert_eq!(out, vec![2.0; 4]);
        assert_eq!(shared.position_frames(), 9004);
    }
}
