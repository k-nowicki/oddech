//! Runs Respiro-en on raw 16 kHz mono f32le audio and prints the segments as
//! JSON: `[[start, end, peak], ...]`. For comparing with `training/`.
//!
//! Usage: respiro_cli audio.f32 [model.onnx]

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).expect("audio file");
    let audio: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    let model = args.get(2).map_or_else(oddech_detect::respiro::model_path, Into::into);
    let t = std::time::Instant::now();
    let r = oddech_detect::respiro::Respiro::load(&model).expect("model");
    eprintln!("loaded in {:.2?} on {}", t.elapsed(), r.device());
    let t = std::time::Instant::now();
    let probs = r.probabilities(&audio, &|_| {}, &|| false).expect("inference");
    eprintln!("{:.1} min of audio in {:.2?}", audio.len() as f64 / 16000.0 / 60.0, t.elapsed());
    let segs = oddech_detect::respiro::segments(&probs);
    println!("{}", serde_json::to_string(&segs).unwrap());
}
