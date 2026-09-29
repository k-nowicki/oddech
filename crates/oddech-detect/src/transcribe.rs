//! Speech transcription with word timestamps (whisper.cpp).

use std::path::Path;
use std::sync::Arc;

use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

use crate::models::WhisperModel;

/// A transcribed word with its time span.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub prob: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    #[error("nie można wczytać modelu mowy: {0}")]
    Load(String),
    #[error("transkrypcja nie powiodła się: {0}")]
    Run(String),
    #[error("przerwano")]
    Cancelled,
}

pub struct Transcriber {
    ctx: WhisperContext,
    pub gpu: bool,
}

impl Transcriber {
    /// Loads the model, on the GPU when possible, falling back to the CPU.
    pub fn load(path: &Path, model: WhisperModel) -> Result<Self, TranscribeError> {
        // Route whisper.cpp / ggml output into the `log` crate.
        whisper_rs::install_logging_hooks();
        let attempt = |gpu: bool| {
            let mut p = WhisperContextParameters::default();
            p.use_gpu(gpu);
            // DTW token timestamps align word boundaries much better than
            // the default heuristic (and are incompatible with flash attn).
            p.flash_attn(false);
            p.dtw_parameters(DtwParameters {
                mode: DtwMode::ModelPreset {
                    model_preset: match model {
                        WhisperModel::LargeV3Turbo => DtwModelPreset::LargeV3Turbo,
                        WhisperModel::LargeV3 => DtwModelPreset::LargeV3,
                    },
                },
                ..Default::default()
            });
            WhisperContext::new_with_params(path, p)
        };
        match attempt(true) {
            Ok(ctx) => Ok(Self { ctx, gpu: true }),
            Err(e) => {
                log::warn!("whisper on GPU failed ({e}); falling back to CPU");
                attempt(false).map(|ctx| Self { ctx, gpu: false }).map_err(|e| TranscribeError::Load(e.to_string()))
            }
        }
    }

    /// Transcribes 16 kHz mono audio (Polish) into words with times in
    /// seconds. `progress` gets values in `[0, 1]`.
    pub fn transcribe(
        &self,
        audio: &[f32],
        progress: Arc<dyn Fn(f32) + Send + Sync>,
        should_abort: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Vec<Word>, TranscribeError> {
        let mut state = self.ctx.create_state().map_err(|e| TranscribeError::Run(e.to_string()))?;
        // Greedy decoding: we need word timing, not the last bit of accuracy.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("pl"));
        params.set_translate(false);
        params.set_token_timestamps(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_n_threads(std::thread::available_parallelism().map_or(4, |n| n.get().min(8)) as i32);
        {
            let p = Arc::clone(&progress);
            params.set_progress_callback_safe(move |pct: i32| p(pct as f32 / 100.0));
        }
        // whisper-rs 0.16's `set_abort_callback_safe` casts its boxed
        // closure to the wrong type (segfault with capturing closures), so
        // the callback is wired up by hand. `abort_box` outlives `full()`.
        unsafe extern "C" fn abort_trampoline(user_data: *mut std::ffi::c_void) -> bool {
            // SAFETY: `user_data` points at `abort_box` below, alive for the
            // whole `full()` call.
            let f = unsafe { &*(user_data as *const Arc<dyn Fn() -> bool + Send + Sync>) };
            f()
        }
        let abort_box: Box<Arc<dyn Fn() -> bool + Send + Sync>> = Box::new(Arc::clone(&should_abort));
        // SAFETY: trampoline and user data match; see above.
        unsafe {
            params.set_abort_callback(Some(abort_trampoline));
            params.set_abort_callback_user_data(&*abort_box as *const _ as *mut std::ffi::c_void);
        }
        let result = state.full(params, audio);
        drop(abort_box);
        result.map_err(|e| {
            if should_abort() { TranscribeError::Cancelled } else { TranscribeError::Run(e.to_string()) }
        })?;
        if should_abort() {
            return Err(TranscribeError::Cancelled);
        }

        let eot = self.ctx.token_eot();
        let mut words: Vec<Word> = Vec::new();
        for segment in state.as_iter() {
            for t in 0..segment.n_tokens() {
                let Some(token) = segment.get_token(t) else { continue };
                let data = token.token_data();
                if data.id >= eot {
                    continue; // special / timestamp tokens
                }
                let text = token.to_str_lossy().map(|s| s.into_owned()).unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                let (t0, t1) = (data.t0 as f64 / 100.0, data.t1 as f64 / 100.0);
                if std::env::var_os("ODDECH_DEBUG_TOKENS").is_some() {
                    eprintln!("tok {:?} t0 {t0:.2} t1 {t1:.2} dtw {:.2} p {:.2}", text, data.t_dtw as f64 / 100.0, data.p);
                }
                let starts_word = text.starts_with(' ') || words.is_empty();
                // Punctuation belongs to the previous word.
                let punct = text.trim().chars().all(|c| c.is_ascii_punctuation());
                match words.last_mut() {
                    Some(w) if !starts_word || punct => {
                        w.text.push_str(&text);
                        if !punct {
                            w.end = w.end.max(t1);
                        }
                        w.prob = w.prob.min(data.p);
                    }
                    _ => words.push(Word { start: t0, end: t1.max(t0), text: text.trim().to_owned(), prob: data.p }),
                }
            }
        }
        words.retain(|w| w.end > w.start);
        progress(1.0);
        Ok(words)
    }
}
