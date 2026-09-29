//! Breath detection with Respiro-en (Yang et al., "Frame-Wise Breath
//! Detection with Self-Training", INTERSPEECH 2024), run through ONNX
//! Runtime.
//!
//! The model gives a breath probability for every 10 ms frame of 16 kHz
//! mono audio. Its input features are reproduced here exactly as the
//! reference implementation computes them with librosa (checked against a
//! fixture in `tests/respiro_features.rs`):
//! * log-mel spectrogram (n_fft 400, hop 160, 128 Slaney mels, periodic
//!   Hann, zero padding), in dB relative to its own maximum, floored 80 dB
//!   below it;
//! * the variance of each frame's mel column, broadcast over the mel axis;
//! * zero-crossing rate of each 25 ms frame, broadcast over the mel axis.
//!
//! Long recordings run in 3-minute windows (the model attends over its
//! whole input, so memory grows with the square of its length). Each window
//! sets its own level reference, which follows the speaker moving relative
//! to the microphone. Windows overlap by half; every frame is taken from
//! the window where it lies closest to the centre.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use realfft::RealFftPlanner;

pub const SAMPLE_RATE: u32 = 16_000;
pub const FRAME_SECS: f64 = 0.01;
const N_FFT: usize = 400;
const HOP: usize = 160;
const N_MELS: usize = 128;
const TOP_DB: f32 = 80.0;
const AMIN: f64 = 1e-10;
const WINDOW_FRAMES: usize = 180 * 100;
/// Frame probability above which a frame counts as breath (the value the
/// authors chose on their validation set).
pub const FRAME_THRESHOLD: f32 = 0.064;
/// Runs must be longer than this many frames (reference implementation).
const MIN_FRAMES: usize = 20;
pub const MODEL_FILE: &str = "respiro-en.onnx";

#[derive(Debug, thiserror::Error)]
pub enum RespiroError {
    #[error("nie znaleziono biblioteki ONNX Runtime ({0})")]
    Runtime(String),
    #[error("nie znaleziono modelu wykrywania wdechów: {0}")]
    MissingModel(PathBuf),
    #[error("model wykrywania wdechów: {0}")]
    Model(String),
    #[error("przerwano")]
    Cancelled,
}

/// Where the exported model lives (see `training/export_respiro_onnx.py`).
pub fn model_path() -> PathBuf {
    crate::models::models_dir().join(MODEL_FILE)
}

/// `libonnxruntime.so`: `ORT_DYLIB_PATH`, else `<app>/lib`.
fn runtime_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        return PathBuf::from(p);
    }
    crate::models::lib_dir().join("libonnxruntime.so")
}

/// Loads ONNX Runtime once per process.
fn init_runtime() -> Result<(), RespiroError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = runtime_path();
        if !path.exists() {
            return Err(path.display().to_string());
        }
        let builder = ort::init_from(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        builder.with_name("oddech").commit();
        // ort releases the environment from an exit handler, which runs
        // after the dynamically loaded runtime has been finalised and
        // crashes. Holding one reference forever skips that release; the
        // OS reclaims everything at exit anyway.
        match ort::environment::Environment::current() {
            Ok(env) => std::mem::forget(env),
            Err(e) => return Err(e.to_string()),
        }
        log::info!("onnx runtime loaded from {}", path.display());
        Ok(())
    })
    .clone()
    .map_err(RespiroError::Runtime)
}

/// Where the model runs. On the CPU, `reason` says why the GPU is not used,
/// keeping the underlying error text so it can be looked up; `fallback`
/// tells a GPU that failed from a machine that has none to use.
#[derive(Debug, Clone, PartialEq)]
pub enum Device {
    Gpu { name: String },
    Cpu { reason: String, fallback: bool },
}

impl Device {
    pub fn is_gpu(&self) -> bool {
        matches!(self, Self::Gpu { .. })
    }
}

impl std::fmt::Display for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gpu { name } => write!(f, "GPU ({name}, CUDA)"),
            Self::Cpu { reason, .. } => write!(f, "CPU ({reason})"),
        }
    }
}

/// Why CUDA must not even be tried, if so. ONNX Runtime aborts the process
/// (instead of failing) when asked for CUDA with no device visible.
fn cuda_blocker() -> Option<String> {
    if std::env::var_os("ODDECH_CPU").is_some_and(|v| v != "0") {
        return Some("forced by ODDECH_CPU".into());
    }
    if std::env::var("CUDA_VISIBLE_DEVICES").is_ok_and(|v| v.trim().is_empty() || v.trim() == "-1") {
        return Some("no GPU visible: CUDA_VISIBLE_DEVICES is empty".into());
    }
    if !Path::new("/dev/nvidiactl").exists() {
        return Some("no NVIDIA driver loaded (/dev/nvidiactl missing)".into());
    }
    // Without its own CUDA provider next to it, ONNX Runtime would load
    // whatever copy the system has, possibly of another version (crash).
    let provider = runtime_path().with_file_name(CUDA_PROVIDER);
    if !provider.exists() {
        return Some(format!("GPU support not installed ({CUDA_PROVIDER} missing)"));
    }
    None
}

const CUDA_PROVIDER: &str = "libonnxruntime_providers_cuda.so";

/// The part of an ONNX Runtime error that names the cause and can be
/// looked up, e.g. "CUDA failure 100: no CUDA-capable device is detected"
/// or "cudaErrorNoKernelImageForDevice: no kernel image is available…".
/// ONNX Runtime prefixes it with source paths and C++ signatures.
fn short_error(e: &str) -> String {
    let e = e.replace('\n', " ");
    // Most specific first; the signatures before the cause mention
    // `cudaError` too, so a named error must follow it.
    let named_cuda_error = || {
        e.match_indices("cudaError").map(|(i, _)| i).find(|&i| e[i + 9..].starts_with(|c: char| c.is_ascii_uppercase()))
    };
    let start = ["CUDA failure ", "CUDNN failure ", "CUBLAS failure "]
        .iter()
        .find_map(|m| e.find(m))
        .or_else(named_cuda_error)
        .or_else(|| {
            // A missing library: keep its name, which precedes the message.
            let i = e.find(": cannot open shared object file")?;
            Some(e[..i].rfind(|c: char| c.is_whitespace() || c == '(' || c == ':').map_or(0, |j| j + 1))
        });
    let text = match start {
        Some(i) => &e[i..],
        None => &e,
    };
    // Drop ONNX Runtime's trailing "; GPU=…; hostname=…; file=…" details.
    let text = text.split(" ; ").next().unwrap_or(text).trim();
    let mut out: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        out.push('…');
    }
    out
}

/// "NVIDIA GeForce RTX 4060 Ti, driver 580.167.08", from the kernel
/// driver's reports.
fn nvidia_description() -> String {
    let model = std::fs::read_dir("/proc/driver/nvidia/gpus")
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| e.ok())
        .and_then(|e| std::fs::read_to_string(e.path().join("information")).ok())
        .and_then(|info| info.lines().find_map(|l| l.strip_prefix("Model:").map(|m| m.trim().to_owned())))
        .unwrap_or_else(|| "NVIDIA GPU".into());
    let driver = std::fs::read_to_string("/proc/driver/nvidia/version").ok().and_then(|v| {
        let line = v.lines().next()?.to_owned();
        let after = line.split("Kernel Module").nth(1)?;
        after.split_whitespace().next().map(str::to_owned)
    });
    match driver {
        Some(d) => format!("{model}, driver {d}"),
        None => model,
    }
}

/// A second of deterministic test audio (tone + noise + silence).
fn self_test_audio() -> Vec<f32> {
    let mut seed = 0x2545_f491_u32;
    (0..SAMPLE_RATE as usize)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.2;
            let tone = (i as f32 * 0.07).sin() * 0.3;
            if i < 8000 { tone + noise } else { noise * 0.1 }
        })
        .collect()
}

/// Largest difference between GPU and CPU output that still counts as the
/// same result (floating-point order differs between the two).
const SELF_TEST_TOLERANCE: f32 = 1e-2;

struct Inner {
    session: ort::session::Session,
    model: PathBuf,
}

impl Inner {
    fn run(&mut self, features: &[f32], t: usize) -> Result<Vec<f32>, String> {
        let input = ort::value::Tensor::from_array(([1usize, 3, N_MELS, t], features.to_vec())).map_err(|e| e.to_string())?;
        let outputs = self.session.run(ort::inputs!["features" => input]).map_err(|e| e.to_string())?;
        let (_, probs) = outputs["probs"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        Ok(probs.to_vec())
    }
}

fn cpu_session(model: &Path) -> Result<ort::session::Session, RespiroError> {
    let err = |e: ort::Error| RespiroError::Model(e.to_string());
    ort::session::Session::builder().map_err(err)?.commit_from_file(model).map_err(err)
}

pub struct Respiro {
    inner: Mutex<Inner>,
    /// Apart from `inner`, which is held for a whole window of detection.
    device: Mutex<Device>,
}

/// State of the shared model, for display.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    NotLoaded,
    Loading,
    Ready(Device),
    Failed(String),
}

static SHARED: Mutex<Option<&'static Respiro>> = Mutex::new(None);
static LOAD_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// State of [`Respiro::shared`], without waiting for a load in progress.
pub fn status() -> Status {
    match SHARED.try_lock() {
        Err(std::sync::TryLockError::WouldBlock) => Status::Loading,
        Err(std::sync::TryLockError::Poisoned(_)) => Status::NotLoaded,
        Ok(shared) => match *shared {
            Some(r) => Status::Ready(r.device()),
            None => match LOAD_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                Some(e) => Status::Failed(e),
                None => Status::NotLoaded,
            },
        },
    }
}

impl Respiro {
    /// The model, loaded on first use and kept for the life of the process.
    ///
    /// ONNX Runtime sessions are never released: in ONNX Runtime 1.22,
    /// releasing a session while a CUDA session exists corrupts the heap
    /// (crash at exit). Loading once also saves the GPU start-up and self
    /// test on every detection.
    pub fn shared() -> Result<&'static Self, RespiroError> {
        let mut shared = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = *shared {
            return Ok(r);
        }
        // A failure is not kept: the next detection tries again.
        let loaded = Self::load(&model_path());
        *LOAD_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = loaded.as_ref().err().map(ToString::to_string);
        let r: &'static Self = Box::leak(Box::new(loaded?));
        *shared = Some(r);
        Ok(r)
    }

    /// Loads the model on the GPU (CUDA) if it is there and passes a self
    /// test against the CPU; otherwise on the CPU. The outcome, with the
    /// reason for using the CPU, is logged and available from [`device`].
    ///
    /// [`device`]: Self::device
    pub fn load(model: &Path) -> Result<Self, RespiroError> {
        if !model.exists() {
            return Err(RespiroError::MissingModel(model.to_owned()));
        }
        init_runtime()?;
        let cpu = cpu_session(model)?;
        let (session, device) = match cuda_blocker() {
            Some(reason) => (cpu, Device::Cpu { reason, fallback: false }),
            None => match Self::gpu_session(model, cpu) {
                Ok((gpu, name)) => (gpu, Device::Gpu { name }),
                Err((cpu, reason)) => (cpu, Device::Cpu { reason, fallback: true }),
            },
        };
        match &device {
            Device::Cpu { fallback: true, .. } => log::warn!("breath detection on {device}"),
            _ => log::info!("breath detection on {device}"),
        }
        Ok(Self { inner: Mutex::new(Inner { session, model: model.to_owned() }), device: Mutex::new(device) })
    }

    /// Opens the model with CUDA and checks that it computes what the CPU
    /// does. Some failures (e.g. a GPU generation the runtime has no kernels
    /// for, `cudaErrorNoKernelImageForDevice`) only show when running.
    /// On failure gives the CPU session back with the reason.
    fn gpu_session(
        model: &Path,
        cpu: ort::session::Session,
    ) -> Result<(ort::session::Session, String), (ort::session::Session, String)> {
        let name = nvidia_description();
        let session = ort::session::Session::builder()
            .map_err(|e| e.to_string())
            .and_then(|b| {
                b.with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()])
                    .map_err(|e| e.to_string())
            })
            .and_then(|mut b| b.commit_from_file(model).map_err(|e| e.to_string()));
        let session = match session {
            Ok(s) => s,
            Err(e) => {
                log::debug!("CUDA session error: {e}");
                return Err((cpu, format!("CUDA could not start on {name}: {}", short_error(&e))));
            }
        };
        let (features, t) = features(&self_test_audio());
        let mut gpu = Inner { session, model: model.to_owned() };
        let gpu_out = match gpu.run(&features, t) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("GPU self-test error: {e}");
                // The failed GPU session is not released (see `Respiro::shared`).
                std::mem::forget(gpu);
                return Err((cpu, format!("GPU self-test failed on {name}: {}", short_error(&e))));
            }
        };
        let mut reference = Inner { session: cpu, model: model.to_owned() };
        let cpu_out = match reference.run(&features, t) {
            Ok(p) => p,
            // The CPU path is broken too; let detection report it.
            Err(_) => {
                std::mem::forget(reference);
                return Ok((gpu.session, name));
            }
        };
        let diff = gpu_out.iter().zip(&cpu_out).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        if gpu_out.len() != cpu_out.len() || diff > SELF_TEST_TOLERANCE {
            std::mem::forget(gpu);
            return Err((
                reference.session,
                format!("GPU self-test failed on {name}: results differ from the CPU by {diff:.4}"),
            ));
        }
        std::mem::forget(reference);
        log::debug!("GPU self-test passed (largest difference from CPU {diff:.2e})");
        Ok((gpu.session, name))
    }

    /// Where the model runs now (it moves to the CPU if the GPU fails).
    pub fn device(&self) -> Device {
        self.device.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Breath probability per 10 ms frame of 16 kHz mono `audio`.
    pub fn probabilities(
        &self,
        audio: &[f32],
        progress: &dyn Fn(f32),
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, RespiroError> {
        let n_frames = audio.len() / HOP + 1;
        let mut out = vec![0.0f32; n_frames];
        let mut best = vec![f32::INFINITY; n_frames];
        let win = WINDOW_FRAMES * HOP;
        let step = win / 2;
        let mut start = 0usize;
        loop {
            if cancelled() {
                return Err(RespiroError::Cancelled);
            }
            let seg = &audio[start..(start + win).min(audio.len())];
            let probs = self.run(seg)?;
            let first = start / HOP;
            let centre = first as f32 + probs.len() as f32 / 2.0;
            for (i, &p) in probs.iter().enumerate() {
                let idx = first + i;
                if idx >= n_frames {
                    break;
                }
                let dist = (idx as f32 - centre).abs();
                if dist < best[idx] {
                    best[idx] = dist;
                    out[idx] = p;
                }
            }
            progress(((start + seg.len()) as f32 / audio.len().max(1) as f32).min(1.0));
            if start + win >= audio.len() {
                break;
            }
            start += step;
        }
        Ok(out)
    }

    /// Runs one window. If the GPU fails mid-way, the model moves to the
    /// CPU (logged with the reason) and the window is run again.
    fn run(&self, audio: &[f32]) -> Result<Vec<f32>, RespiroError> {
        let (data, t) = features(audio);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.run(&data, t) {
            Ok(p) => Ok(p),
            Err(e) if self.device().is_gpu() => {
                let Device::Gpu { name } = self.device() else { unreachable!() };
                log::debug!("GPU error: {e}");
                let reason = format!("GPU failed during detection on {name}: {}", short_error(&e));
                log::warn!("breath detection moves to the CPU ({reason})");
                let cpu = cpu_session(&inner.model)?;
                let gpu = std::mem::replace(&mut inner.session, cpu);
                std::mem::forget(gpu);
                *self.device.lock().unwrap_or_else(|e| e.into_inner()) = Device::Cpu { reason, fallback: true };
                inner.run(&data, t).map_err(RespiroError::Model)
            }
            Err(e) => Err(RespiroError::Model(e)),
        }
    }
}

/// Runs of breath frames: `(start s, end s, peak probability)`.
pub fn segments(probs: &[f32]) -> Vec<(f64, f64, f32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < probs.len() {
        if probs[i] <= FRAME_THRESHOLD {
            i += 1;
            continue;
        }
        let first = i;
        let mut peak = 0.0f32;
        while i < probs.len() && probs[i] > FRAME_THRESHOLD {
            peak = peak.max(probs[i]);
            i += 1;
        }
        if i - first > MIN_FRAMES {
            out.push((first as f64 * FRAME_SECS, i as f64 * FRAME_SECS, peak));
        }
    }
    out
}

/// Model input `[1, 3, 128, T]` (row-major) and `T`.
pub fn features(audio: &[f32]) -> (Vec<f32>, usize) {
    let t = audio.len() / HOP + 1;
    let mel = log_mel(audio, t);
    let mut data = vec![0.0f32; 3 * N_MELS * t];
    data[..N_MELS * t].copy_from_slice(&mel);
    // Unbiased variance over the mel axis (torch.var default).
    let zcr = zero_crossing_rate(audio, t);
    for f in 0..t {
        let col = (0..N_MELS).map(|m| f64::from(mel[m * t + f]));
        let mean = col.clone().sum::<f64>() / N_MELS as f64;
        let var = col.map(|v| (v - mean).powi(2)).sum::<f64>() / (N_MELS - 1) as f64;
        for m in 0..N_MELS {
            data[N_MELS * t + m * t + f] = var as f32;
            data[2 * N_MELS * t + m * t + f] = zcr[f];
        }
    }
    (data, t)
}

/// Mel power spectrogram in dB (`librosa.power_to_db(ref=np.max)`), laid
/// out mel-major: `[m * t + frame]`.
fn log_mel(audio: &[f32], t: usize) -> Vec<f32> {
    let filters = mel_filters();
    let window: Vec<f64> =
        (0..N_FFT).map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / N_FFT as f64).cos()).collect();
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut power = vec![0.0f64; N_FFT / 2 + 1];
    let mut mel = vec![0.0f64; N_MELS * t];
    let pad = N_FFT / 2;
    for f in 0..t {
        for (n, x) in input.iter_mut().enumerate() {
            // Centred frames over zero-padded audio.
            let i = (f * HOP + n).wrapping_sub(pad);
            *x = audio.get(i).map_or(0.0, |&s| f64::from(s)) * window[n];
        }
        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch).expect("fft sizes are fixed");
        for (p, c) in power.iter_mut().zip(&spectrum) {
            *p = c.norm_sqr();
        }
        for (m, row) in filters.iter().enumerate() {
            mel[m * t + f] = row.iter().map(|&(k, w)| w * power[k]).sum();
        }
    }
    let reference = mel.iter().copied().fold(0.0f64, f64::max).max(AMIN);
    let ref_db = 10.0 * reference.log10();
    let mut db: Vec<f32> = mel.iter().map(|&v| (10.0 * v.max(AMIN).log10() - ref_db) as f32).collect();
    let floor = db.iter().copied().fold(f32::NEG_INFINITY, f32::max) - TOP_DB;
    for v in &mut db {
        *v = v.max(floor);
    }
    db
}

/// Zero-crossing rate as in Respiro-en's `zcr_extractor`: 25 ms frames,
/// centred with zero padding, `sum |sign(x[n+1]) - sign(x[n])| / 2 / 400`.
fn zero_crossing_rate(audio: &[f32], t: usize) -> Vec<f32> {
    let pad = N_FFT / 2;
    let sample = |i: usize| i.checked_sub(pad).and_then(|i| audio.get(i)).map_or(0.0, |&s| s.signum_zero());
    (0..t)
        .map(|f| {
            let s = f * HOP;
            let crossings: f32 = (s..s + N_FFT - 1).map(|i| (sample(i + 1) - sample(i)).abs()).sum();
            crossings * 0.5 / N_FFT as f32
        })
        .collect()
}

trait SignumZero {
    fn signum_zero(self) -> f32;
}

impl SignumZero for f32 {
    /// `np.sign`: 0 for 0.
    fn signum_zero(self) -> f32 {
        if self > 0.0 {
            1.0
        } else if self < 0.0 {
            -1.0
        } else {
            0.0
        }
    }
}

/// `librosa.filters.mel(sr=16000, n_fft=400, n_mels=128)` (Slaney scale and
/// normalisation) as sparse rows of `(fft bin, weight)`.
fn mel_filters() -> &'static [Vec<(usize, f64)>] {
    static FILTERS: OnceLock<Vec<Vec<(usize, f64)>>> = OnceLock::new();
    FILTERS.get_or_init(|| {
        let sr = f64::from(SAMPLE_RATE);
        let n_bins = N_FFT / 2 + 1;
        let fft_freqs: Vec<f64> = (0..n_bins).map(|k| k as f64 * sr / N_FFT as f64).collect();
        let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(sr / 2.0));
        let mel_f: Vec<f64> =
            (0..N_MELS + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (N_MELS + 1) as f64)).collect();
        (0..N_MELS)
            .map(|m| {
                let enorm = 2.0 / (mel_f[m + 2] - mel_f[m]);
                let (lower_w, upper_w) = (mel_f[m + 1] - mel_f[m], mel_f[m + 2] - mel_f[m + 1]);
                fft_freqs
                    .iter()
                    .enumerate()
                    .filter_map(|(k, &f)| {
                        let lower = (f - mel_f[m]) / lower_w;
                        let upper = (mel_f[m + 2] - f) / upper_w;
                        let w = lower.min(upper).max(0.0) * enorm;
                        // librosa builds the matrix in float32.
                        let w = f64::from(w as f32);
                        (w > 0.0).then_some((k, w))
                    })
                    .collect()
            })
            .collect()
    })
}

const F_SP: f64 = 200.0 / 3.0;
const MIN_LOG_HZ: f64 = 1000.0;
const MIN_LOG_MEL: f64 = MIN_LOG_HZ / F_SP;

fn log_step() -> f64 {
    6.4f64.ln() / 27.0
}

fn hz_to_mel(f: f64) -> f64 {
    if f >= MIN_LOG_HZ { MIN_LOG_MEL + (f / MIN_LOG_HZ).ln() / log_step() } else { f / F_SP }
}

fn mel_to_hz(m: f64) -> f64 {
    if m >= MIN_LOG_MEL { MIN_LOG_HZ * (log_step() * (m - MIN_LOG_MEL)).exp() } else { F_SP * m }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_follow_reference_rules() {
        let mut p = vec![0.0f32; 200];
        p[10..40].fill(0.9); // 30 frames: kept
        p[50..65].fill(0.9); // 15 frames: too short
        p[100..121].fill(0.2); // 21 frames: kept, peak 0.2
        p[110] = 0.7;
        let s = segments(&p);
        assert_eq!(s.len(), 2);
        assert!((s[0].0 - 0.10).abs() < 1e-9 && (s[0].1 - 0.40).abs() < 1e-9);
        assert_eq!(s[1].2, 0.7);
    }

    #[test]
    fn mel_scale_round_trips() {
        for f in [0.0, 440.0, 999.0, 1000.0, 4000.0, 8000.0] {
            assert!((mel_to_hz(hz_to_mel(f)) - f).abs() < 1e-6);
        }
    }

    #[test]
    fn short_error_keeps_the_searchable_cause() {
        let sig = "/onnxruntime_src/onnxruntime/core/providers/cuda/cuda_call.cc:129 std::conditional_t<THRW, void, onnxruntime::common::Status> onnxruntime::CudaCall(ERRTYPE, const char*, const char*, SUCCTYPE, const char*, const char*, int) [with ERRTYPE = cudaError; bool THRW = true; SUCCTYPE = cudaError; std::conditional_t<THRW, void, common::Status> = void] ";
        let no_device = format!("{sig}CUDA failure 100: no CUDA-capable device is detected ; GPU=-1 ; hostname=h ; file=x.cc ; line=282 ; expr=cudaSetDevice(0); \n\n");
        assert_eq!(short_error(&no_device), "CUDA failure 100: no CUDA-capable device is detected");
        let no_kernel = "Non-zero status code returned while running Slice node. Name:'Slice_34' Status Message: CUDA error cudaErrorNoKernelImageForDevice:no kernel image is available for execution on the device";
        assert_eq!(short_error(no_kernel), "cudaErrorNoKernelImageForDevice:no kernel image is available for execution on the device");
        let no_lib = "Failed to load library libonnxruntime_providers_cuda.so with error: libcudnn.so.9: cannot open shared object file: No such file or directory";
        assert_eq!(short_error(no_lib), "libcudnn.so.9: cannot open shared object file: No such file or directory");
        assert_eq!(short_error("something else"), "something else");
    }
}
