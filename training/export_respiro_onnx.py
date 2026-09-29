"""Exports Respiro-en to ONNX for the app and checks it against PyTorch.

Input:  `features` float32 [1, 3, 128, T] (log-mel, its per-frame variance,
        zero-crossing rate; see crates/oddech-detect/src/respiro.rs)
Output: `probs`    float32 [1, T]   breath probability per 10 ms frame

Also writes a feature fixture for the Rust test that checks the feature
extraction matches librosa.

Usage: python export_respiro_onnx.py [out.onnx] [recording]
       out.onnx default: <repo>/models/respiro-en.onnx (the model shipped
       with the app). With a recording, ONNX is also checked against PyTorch on
       its first audio track; otherwise on synthetic audio.
"""

import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort
import torch

import eval_respiro as e
from modules import feature_extractor  # noqa: E402  (path set by eval_respiro)

FIXTURE = e.HERE.parent / "crates" / "oddech-detect" / "tests" / "fixtures"


class Wrapper(torch.nn.Module):
    def __init__(self, net):
        super().__init__()
        self.net = net

    def forward(self, features):
        length = torch.full((1,), features.shape[-1], dtype=torch.long)
        return self.net(features, length)


def fixture_audio() -> np.ndarray:
    """3 s of deterministic test audio: noise bursts, a tone, silence."""
    rng = np.random.default_rng(7)
    t = np.arange(3 * e.SR) / e.SR
    wav = 0.02 * rng.standard_normal(len(t))
    wav += 0.3 * np.sin(2 * np.pi * 220 * t) * (t > 1.0) * (t < 2.0)
    wav[int(2.5 * e.SR):] = 0.0
    return wav.astype(np.float32)


def main():
    recording = Path(sys.argv[2]) if len(sys.argv) > 2 else None
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else e.HERE.parent / "models" / "respiro-en.onnx"
    out.parent.mkdir(parents=True, exist_ok=True)
    model = e.load_model(torch.device("cpu"))
    wrapper = Wrapper(model).eval()

    dummy = torch.randn(1, 3, 128, 1000)
    frames = torch.export.Dim("frames", min=64, max=40000)
    torch.onnx.export(wrapper, (dummy,), str(out), input_names=["features"], output_names=["probs"],
                      dynamic_shapes={"features": {3: frames}}, opset_version=18, dynamo=True,
                      external_data=False)
    print(f"zapisano {out} ({out.stat().st_size / 1e6:.1f} MB)")

    # Same results as PyTorch on real audio of different lengths.
    sess = ort.InferenceSession(str(out), providers=["CPUExecutionProvider"])
    if recording:
        wav = e.load_audio(recording, e.audio_streams(recording)[0])
    else:
        rng = np.random.default_rng(1)
        wav = (0.05 * rng.standard_normal(180 * e.SR)).astype(np.float32)
    for seconds in (7, 60, 180):
        f, _ = feature_extractor(wav[: seconds * e.SR], e.SR)
        with torch.no_grad():
            ref = wrapper(f).numpy()
        got = sess.run(None, {"features": f.numpy()})[0]
        print(f"  {seconds:3} s: max różnica ONNX vs PyTorch {np.abs(ref - got).max():.2e}")

    # Feature fixture for the Rust implementation.
    FIXTURE.mkdir(parents=True, exist_ok=True)
    audio = fixture_audio()
    f, _ = feature_extractor(audio, e.SR)
    audio.tofile(FIXTURE / "respiro_audio.f32")
    f.numpy().astype(np.float32).tofile(FIXTURE / "respiro_features.f32")
    print(f"fixture: {f.shape}")


if __name__ == "__main__":
    main()
