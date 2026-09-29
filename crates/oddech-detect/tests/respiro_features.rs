//! The Rust feature extraction must match the reference implementation
//! (librosa + Respiro-en's code). Fixture written by
//! `training/export_respiro_onnx.py`.

use oddech_detect::respiro::features;

fn read_f32(name: &str) -> Vec<f32> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read(path).unwrap().chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
}

#[test]
fn features_match_librosa() {
    let audio = read_f32("respiro_audio.f32");
    let expected = read_f32("respiro_features.f32");
    let (got, t) = features(&audio);
    assert_eq!(got.len(), expected.len(), "frames: {t}");
    let block = 128 * t;
    for (ch, name, tol) in [(0, "log-mel dB", 0.01), (1, "mel variance", 0.05), (2, "zcr", 1e-6)] {
        let worst = (ch * block..(ch + 1) * block)
            .map(|i| (got[i] - expected[i]).abs())
            .fold(0.0f32, f32::max);
        println!("{name}: max difference {worst}");
        assert!(worst <= tol, "{name}: max difference {worst} > {tol}");
    }
}
