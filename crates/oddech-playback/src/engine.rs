//! The playback engine: owns the output stream and the feeder thread.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use oddech_core::store::TrackStore;

use crate::mixer::{MixSettings, Mixer, PlayRequest};
use crate::transport::{Consumer, Producer, TransportShared, transport};

/// Frames rendered per feeder iteration (~5 ms at 48 kHz).
const BLOCK_FRAMES: usize = 256;
/// Queue depth: bounds the latency of A/B switches and gain changes
/// (~64 ms at 48 kHz) while absorbing scheduling jitter of the feeder.
const QUEUE_FRAMES: usize = 3072;
const FLUSH_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("brak urządzenia wyjściowego audio")]
    NoDevice,
    #[error("błąd konfiguracji urządzenia audio: {0}")]
    Config(String),
    #[error("nie udało się uruchomić strumienia audio: {0}")]
    Stream(String),
}

enum Command {
    Play(PlayRequest),
    Stop,
}

struct FeederShared {
    mix: ArcSwap<MixSettings>,
    active: AtomicBool,
    /// Commands sent but not yet processed by the feeder. While a Play is
    /// in flight the engine must already report "playing", otherwise the UI
    /// may observe a stopped engine right after pressing play.
    pending: AtomicUsize,
}

pub struct PlaybackEngine {
    commands: Sender<Command>,
    transport: Arc<TransportShared>,
    feeder_shared: Arc<FeederShared>,
    feeder: Option<JoinHandle<()>>,
    device_rate: u32,
    device_name: String,
    duration: f64,
    last_command_play: AtomicBool,
    _stream: Option<cpal::Stream>,
}

impl PlaybackEngine {
    /// Opens the default output device and starts the (idle) stream.
    pub fn new(stores: Vec<Arc<TrackStore>>) -> Result<Self, PlaybackError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(PlaybackError::NoDevice)?;
        let device_name = device
            .description()
            .map(|d| d.to_string())
            .unwrap_or_else(|_| "?".into());
        let supported = device
            .default_output_config()
            .map_err(|e| PlaybackError::Config(e.to_string()))?;
        let rate = supported.sample_rate();
        let channels = usize::from(supported.channels());
        log::info!(
            "audio output: {device_name}, {rate} Hz, {channels} ch, {:?}",
            supported.sample_format()
        );

        let (producer, consumer, transport_shared) = transport(QUEUE_FRAMES, channels);
        let mut engine = Self::spawn(stores, rate, device_name, producer, transport_shared);

        let mut config: cpal::StreamConfig = supported.config();
        config.buffer_size = cpal::BufferSize::Fixed(512);
        let stream = match build_stream(&device, &config, supported.sample_format(), consumer) {
            Ok(s) => s,
            Err((e, consumer)) => {
                log::warn!("fixed buffer size rejected ({e}); using device default");
                config.buffer_size = cpal::BufferSize::Default;
                build_stream(&device, &config, supported.sample_format(), consumer)
                    .map_err(|(e, _)| PlaybackError::Stream(e))?
            }
        };
        stream.play().map_err(|e| PlaybackError::Stream(e.to_string()))?;
        engine._stream = Some(stream);
        Ok(engine)
    }

    /// An engine without an audio device; the caller drives the returned
    /// consumer. Used by tests and offline tools.
    pub fn headless(stores: Vec<Arc<TrackStore>>, rate: u32, channels: usize) -> (Self, Consumer) {
        let (producer, consumer, shared) = transport(QUEUE_FRAMES, channels);
        (Self::spawn(stores, rate, "headless".into(), producer, shared), consumer)
    }

    fn spawn(
        stores: Vec<Arc<TrackStore>>,
        rate: u32,
        device_name: String,
        producer: Producer,
        transport: Arc<TransportShared>,
    ) -> Self {
        let mixer = Mixer::new(stores, rate, producer.channels());
        let duration = mixer.duration();
        let feeder_shared = Arc::new(FeederShared {
            mix: ArcSwap::from_pointee(MixSettings::default()),
            active: AtomicBool::new(false),
            pending: AtomicUsize::new(0),
        });
        let (tx, rx) = unbounded();
        let feeder = {
            let fs = Arc::clone(&feeder_shared);
            let ts = Arc::clone(&transport);
            std::thread::Builder::new()
                .name("oddech-feeder".into())
                .spawn(move || Feeder { mixer, producer, rx, shared: fs, transport: ts }.run())
                .expect("spawn feeder thread")
        };
        Self {
            commands: tx,
            transport,
            feeder_shared,
            feeder: Some(feeder),
            device_rate: rate,
            device_name,
            duration,
            last_command_play: AtomicBool::new(false),
            _stream: None,
        }
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Replaces mix settings; takes effect within one queue length.
    pub fn set_mix(&self, mix: MixSettings) {
        self.feeder_shared.mix.store(Arc::new(mix));
    }

    pub fn play(&self, req: PlayRequest) {
        self.last_command_play.store(true, Ordering::Release);
        // Show the new position immediately, before audio starts.
        self.transport.set_position_frames(self.secs_to_frame(req.start));
        self.send(Command::Play(req));
    }

    pub fn stop(&self) {
        self.last_command_play.store(false, Ordering::Release);
        self.send(Command::Stop);
    }

    fn send(&self, cmd: Command) {
        self.feeder_shared.pending.fetch_add(1, Ordering::AcqRel);
        if self.commands.send(cmd).is_err() {
            self.feeder_shared.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Moves the playhead while stopped (no audio).
    pub fn set_position(&self, secs: f64) {
        self.transport.set_position_frames(self.secs_to_frame(secs));
    }

    /// Timeline position of the audio currently being handed to the device.
    pub fn position(&self) -> f64 {
        self.transport.position_frames() as f64 / f64::from(self.device_rate)
    }

    pub fn is_playing(&self) -> bool {
        let pending_play = self.feeder_shared.pending.load(Ordering::Acquire) > 0 && self.last_was_play();
        pending_play || self.feeder_shared.active.load(Ordering::Acquire) || self.transport.is_audible()
    }

    fn last_was_play(&self) -> bool {
        self.last_command_play.load(Ordering::Acquire)
    }

    /// Frames of real audio delivered to the device since start.
    pub fn frames_played(&self) -> u64 {
        self.transport.frames_played()
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    fn secs_to_frame(&self, secs: f64) -> i64 {
        (secs.max(0.0) * f64::from(self.device_rate)).round() as i64
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        // Closing the channel ends the feeder loop.
        let (dead_tx, _) = unbounded();
        drop(std::mem::replace(&mut self.commands, dead_tx));
        if let Some(h) = self.feeder.take() {
            let _ = h.join();
        }
    }
}

type BuildError = (String, Consumer);

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    consumer: Consumer,
) -> Result<cpal::Stream, BuildError> {
    use cpal::SampleFormat as F;
    match format {
        F::F32 => build_typed::<f32>(device, config, consumer),
        F::I16 => build_typed::<i16>(device, config, consumer),
        F::U16 => build_typed::<u16>(device, config, consumer),
        F::I32 => build_typed::<i32>(device, config, consumer),
        other => Err((format!("nieobsługiwany format próbek {other:?}"), consumer)),
    }
}

fn build_typed<T>(device: &cpal::Device, config: &cpal::StreamConfig, consumer: Consumer) -> Result<cpal::Stream, BuildError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    // The consumer must survive a failed attempt so we can retry with a
    // different configuration; share it through a mutex that is only ever
    // locked by the audio callback (uncontended, hence real-time safe).
    let consumer = Arc::new(parking_lot_free::SpinCell::new(consumer));
    let cb_consumer = Arc::clone(&consumer);
    let mut scratch: Vec<f32> = vec![0.0; 1 << 16];
    let result = device.build_output_stream::<T, _, _>(
        config.clone(),
        move |out: &mut [T], _info| {
            let mut c = cb_consumer.lock();
            if scratch.len() < out.len() {
                // Only on a pathological buffer size; one-off allocation.
                scratch.resize(out.len(), 0.0);
            }
            let buf = &mut scratch[..out.len()];
            c.fill(buf);
            for (o, x) in out.iter_mut().zip(buf.iter()) {
                *o = T::from_sample(*x);
            }
        },
        {
            let mut errors = ErrorLog::default();
            move |e| errors.report(&e.to_string())
        },
        None,
    );
    match result {
        Ok(stream) => Ok(stream),
        Err(e) => {
            // The failed stream dropped its closure, so we are the sole owner.
            let consumer = Arc::try_unwrap(consumer)
                .map_err(|_| ())
                .expect("consumer still shared after failed build")
                .into_inner();
            Err((e.to_string(), consumer))
        }
    }
}

/// Logs stream errors without flooding: some ALSA setups (e.g. the
/// PipeWire plugin while a stream connects or recovers) report the same
/// error dozens of times in a row. A repeat within [`ErrorLog::WINDOW`] is
/// only counted; the count is logged with the next different error or once
/// the window has passed.
#[derive(Default)]
struct ErrorLog {
    last: Option<(String, std::time::Instant)>,
    repeats: u32,
}

impl ErrorLog {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

    fn report(&mut self, message: &str) {
        let now = std::time::Instant::now();
        if let Some((last, since)) = &self.last
            && last == message
            && now.duration_since(*since) < Self::WINDOW
        {
            self.repeats += 1;
            return;
        }
        self.flush();
        log::error!("audio stream error: {message}");
        self.last = Some((message.to_owned(), now));
    }

    fn flush(&mut self) {
        if self.repeats > 0 {
            log::error!("audio stream error repeated {} more times", self.repeats);
            self.repeats = 0;
        }
    }
}

impl Drop for ErrorLog {
    fn drop(&mut self) {
        self.flush();
    }
}

/// A tiny uncontended lock for moving the consumer into the callback.
mod parking_lot_free {
    use std::cell::UnsafeCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub struct SpinCell<T> {
        locked: AtomicBool,
        value: UnsafeCell<T>,
    }

    // SAFETY: access to `value` is serialized by `locked`.
    unsafe impl<T: Send> Sync for SpinCell<T> {}

    impl<T> SpinCell<T> {
        pub fn new(value: T) -> Self {
            Self { locked: AtomicBool::new(false), value: UnsafeCell::new(value) }
        }

        pub fn lock(&self) -> Guard<'_, T> {
            while self.locked.swap(true, Ordering::Acquire) {
                std::hint::spin_loop();
            }
            Guard { cell: self }
        }

        pub fn into_inner(self) -> T {
            self.value.into_inner()
        }
    }

    pub struct Guard<'a, T> {
        cell: &'a SpinCell<T>,
    }

    impl<T> std::ops::Deref for Guard<'_, T> {
        type Target = T;
        fn deref(&self) -> &T {
            // SAFETY: we hold the lock.
            unsafe { &*self.cell.value.get() }
        }
    }

    impl<T> std::ops::DerefMut for Guard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            // SAFETY: we hold the lock.
            unsafe { &mut *self.cell.value.get() }
        }
    }

    impl<T> Drop for Guard<'_, T> {
        fn drop(&mut self) {
            self.cell.locked.store(false, Ordering::Release);
        }
    }
}

struct Cursor {
    next_frame: i64,
    end_frame: i64,
    loop_start: Option<i64>,
}

struct Feeder {
    mixer: Mixer,
    producer: Producer,
    rx: Receiver<Command>,
    shared: Arc<FeederShared>,
    transport: Arc<TransportShared>,
}

impl Feeder {
    fn to_frame(&self, secs: f64) -> i64 {
        (secs.max(0.0) * f64::from(self.mixer.device_rate())).round() as i64
    }

    fn handle(&mut self, cmd: Command, cursor: &mut Option<Cursor>) {
        self.producer.flush(FLUSH_TIMEOUT);
        match cmd {
            Command::Play(req) => {
                let start = self.to_frame(req.start);
                let total = self.to_frame(self.mixer.duration());
                let end = req.end.map_or(total, |e| self.to_frame(e)).min(total).max(start);
                self.transport.set_position_frames(start);
                *cursor = Some(Cursor {
                    next_frame: start,
                    end_frame: end,
                    loop_start: req.loop_start.map(|l| self.to_frame(l)).filter(|&l| l < end),
                });
                self.shared.active.store(true, Ordering::Release);
            }
            Command::Stop => {
                *cursor = None;
                self.shared.active.store(false, Ordering::Release);
            }
        }
    }

    fn run(mut self) {
        let mut cursor: Option<Cursor> = None;
        let mut block = Vec::with_capacity(BLOCK_FRAMES * self.producer.channels());
        loop {
            let wait = match &cursor {
                Some(_) if self.producer.free_frames() >= BLOCK_FRAMES => Duration::ZERO,
                Some(_) => Duration::from_millis(2),
                None => Duration::from_millis(100),
            };
            match self.rx.recv_timeout(wait) {
                Ok(mut cmd) => {
                    // Only the latest command matters (e.g. a burst of seeks
                    // while scrubbing): skip superseded ones.
                    let mut taken = 1;
                    while let Ok(next) = self.rx.try_recv() {
                        cmd = next;
                        taken += 1;
                    }
                    self.handle(cmd, &mut cursor);
                    self.shared.pending.fetch_sub(taken, Ordering::AcqRel);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }

            while let Some(c) = &mut cursor {
                if self.producer.free_frames() < BLOCK_FRAMES {
                    break;
                }
                let n = (BLOCK_FRAMES as i64).min(c.end_frame - c.next_frame);
                if n > 0 {
                    let mix = self.shared.mix.load();
                    self.mixer.render(&mix, c.next_frame, n as usize, &mut block);
                    self.producer.push_block(c.next_frame, &block);
                    c.next_frame += n;
                }
                if c.next_frame >= c.end_frame {
                    match c.loop_start {
                        Some(l) => c.next_frame = l,
                        None => {
                            cursor = None;
                            self.shared.active.store(false, Ordering::Release);
                        }
                    }
                }
                // Stay responsive to commands.
                if !self.rx.is_empty() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::TrackMix;

    #[test]
    fn no_drift_over_ten_minutes() {
        // 10 min of audio consumed in odd-sized callbacks. The feeder is
        // paced for real time, so a low rate keeps the test fast; the
        // position arithmetic is rate-independent.
        let dir = tempfile::tempdir().unwrap();
        let rate = 2_000u32;
        let secs = 600.0;
        let start = 12.345;
        let len = ((start + secs + 1.0) * f64::from(rate)) as usize;
        let store = Arc::new(TrackStore::from_vecs(dir.path(), rate, &[vec![0.0; len]]).unwrap());
        let (engine, mut consumer) = PlaybackEngine::headless(vec![store], rate, 2);
        engine.set_mix(MixSettings {
            tracks: vec![TrackMix { audible: true, envelope: None }],
            apply_attenuation: false,
        });
        engine.play(PlayRequest { start, end: None, loop_start: None });
        let mut buf = vec![0.0f32; 2 * 37];
        let start_frame = (start * f64::from(rate)).round() as i64;
        // Wait for the feeder to start producing.
        while engine.frames_played() == 0 {
            consumer.fill(&mut buf);
            std::thread::yield_now();
        }
        let target = (secs * f64::from(rate)) as u64;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while engine.frames_played() < target {
            assert!(std::time::Instant::now() < deadline, "playback stalled");
            consumer.fill(&mut buf);
            let expected = start_frame + engine.frames_played() as i64;
            assert_eq!(engine.transport.position_frames(), expected);
        }
        let pos = engine.position();
        let expected = start + engine.frames_played() as f64 / f64::from(rate);
        assert!((pos - expected).abs() < 1.0 / f64::from(rate), "{pos} vs {expected}");
        engine.stop();
    }

    #[test]
    fn error_log_counts_repeats_instead_of_logging_them() {
        let mut log = ErrorLog::default();
        log.report("I/O error");
        log.report("I/O error");
        log.report("I/O error");
        assert_eq!(log.repeats, 2);
        log.report("underrun");
        assert_eq!(log.repeats, 0);
        assert_eq!(log.last.as_ref().map(|(m, _)| m.as_str()), Some("underrun"));
    }

    #[test]
    fn reports_playing_immediately_after_play() {
        // Regression: the UI polls right after pressing play; the engine must
        // not report "stopped" before the feeder has seen the command.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(TrackStore::from_vecs(dir.path(), 1000, &[vec![0.1; 10_000]]).unwrap());
        let (engine, mut consumer) = PlaybackEngine::headless(vec![store], 1000, 1);
        let stop = Arc::new(AtomicBool::new(false));
        let pump = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut buf = vec![0.0f32; 16];
                while !stop.load(Ordering::Relaxed) {
                    consumer.fill(&mut buf);
                    std::thread::sleep(Duration::from_micros(200));
                }
            })
        };
        for _ in 0..100 {
            engine.play(PlayRequest::from(1.0));
            assert!(engine.is_playing());
            engine.stop();
            engine.play(PlayRequest::from(2.0));
            assert!(engine.is_playing());
        }
        let t = std::time::Instant::now();
        drop(engine);
        assert!(t.elapsed() < Duration::from_secs(2), "shutdown took {:?}", t.elapsed());
        stop.store(true, Ordering::Relaxed);
        pump.join().unwrap();
    }

    #[test]
    fn stops_at_range_end_and_loops() {
        let dir = tempfile::tempdir().unwrap();
        let rate = 1000u32;
        let store = Arc::new(TrackStore::from_vecs(dir.path(), rate, &[vec![0.25; 10_000]]).unwrap());
        let (engine, mut consumer) = PlaybackEngine::headless(vec![store], rate, 1);
        engine.set_mix(MixSettings {
            tracks: vec![TrackMix { audible: true, envelope: None }],
            apply_attenuation: false,
        });
        engine.play(PlayRequest { start: 1.0, end: Some(1.5), loop_start: None });
        let mut buf = vec![0.0f32; 100];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while engine.frames_played() < 500 && std::time::Instant::now() < deadline {
            consumer.fill(&mut buf);
        }
        for _ in 0..50 {
            consumer.fill(&mut buf);
        }
        assert_eq!(engine.frames_played(), 500);
        assert_eq!(engine.position(), 1.5);
        assert!(!engine.is_playing());

        engine.play(PlayRequest { start: 2.0, end: Some(2.3), loop_start: Some(2.0) });
        while engine.frames_played() < 500 + 1000 {
            consumer.fill(&mut buf);
            let p = engine.position();
            assert!((2.0..=2.3).contains(&p), "position {p} outside loop");
        }
        assert!(engine.is_playing());
    }
}
