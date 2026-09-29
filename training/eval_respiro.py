"""Evaluate the pretrained Respiro-en breath detector against reference labels.

Reference labels are the read-only `<media>.oddech.reference.json` files the
app writes on "Zatwierdź wszystkie". Every region of kind `breath` on a track
with `fix` on is ground truth; the user marked all breaths on those tracks.

The app's heuristic is scored from the same file: its proposals are the
regions that carry a model confidence at or above the project threshold.

Usage: python eval_respiro.py <media_dir>
Requires training/vendor/Respiro-en (git clone of ydqmkkx/Respiro-en).
"""

import json
import subprocess
import sys
from pathlib import Path

import numpy as np
import torch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "vendor" / "Respiro-en"))
import librosa  # noqa: E402
from modules import DetectionNet, feature_extractor, zcr_extractor  # noqa: E402

SR = 16000
HOP = 0.01  # Respiro-en frame hop (s)


def load_audio(media: Path, stream: int) -> np.ndarray:
    """Decodes one audio stream to 16 kHz mono float32."""
    cmd = ["ffmpeg", "-v", "error", "-i", str(media), "-map", f"0:{stream}",
           "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"]
    raw = subprocess.run(cmd, check=True, capture_output=True).stdout
    return np.frombuffer(raw, dtype=np.float32)


def audio_streams(media: Path) -> list[int]:
    out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "a", "-show_entries",
                          "stream=index", "-of", "csv=p=0", str(media)],
                         check=True, capture_output=True, text=True).stdout
    return [int(x) for x in out.split()]


def load_model(device):
    model = DetectionNet().to(device)
    ckpt = torch.load(HERE / "vendor" / "Respiro-en" / "respiro-en.pt", map_location=device)
    model.load_state_dict(ckpt["model"])
    model.eval()
    return model


def mel_power(wav):
    return librosa.feature.melspectrogram(y=wav, sr=SR, n_fft=int(SR * 0.025), hop_length=int(SR * 0.01), n_mels=128)


def features_with_ref(wav, ref_power):
    """Respiro-en's feature_extractor, but with a fixed dB reference instead
    of the input's own maximum."""
    mel = librosa.power_to_db(mel_power(wav), ref=ref_power)
    zcr = zcr_extractor(wav, win_length=int(SR * 0.025), hop_length=int(SR * 0.01))
    mel = torch.tensor(mel).unsqueeze(0)
    zcr = torch.tensor(zcr).unsqueeze(0).unsqueeze(1).expand(-1, 128, -1)
    vms = torch.var(mel, dim=1).unsqueeze(1).expand(-1, mel.shape[1], -1)
    feature = torch.stack((mel, vms, zcr), dim=1)
    return feature, torch.tensor([zcr.shape[-1]])


def global_ref(wav) -> float:
    """Maximum mel power over the whole recording (what whole-file mode uses)."""
    step = 60 * SR
    return max(float(mel_power(wav[i:i + step]).max()) for i in range(0, len(wav), step))


@torch.no_grad()
def frame_probs(model, wav: np.ndarray, device, chunk_s: float | None, ref_power: float | None = None) -> np.ndarray:
    """Per-10 ms breath probability. With `chunk_s`, the file is processed in
    overlapping windows (the model was trained on short utterances and
    normalizes the mel spectrogram to each input's maximum); each frame is
    taken from the window where it is closest to the centre."""
    n_frames = len(wav) // int(SR * HOP) + 1
    if chunk_s is None:
        f, l = feature_extractor(wav, SR)
        return model(f.to(device), l.to(device))[0].cpu().numpy()[:n_frames]
    win = int(chunk_s * SR)
    hop = win // 2
    out = np.zeros(n_frames, dtype=np.float32)
    best = np.full(n_frames, np.inf)
    start = 0
    while True:
        seg = wav[start:start + win]
        f, l = feature_extractor(seg, SR) if ref_power is None else features_with_ref(seg, ref_power)
        p = model(f.to(device), l.to(device))[0].cpu().numpy()
        first = start // int(SR * HOP)
        idx = np.arange(len(p)) + first
        keep = idx < n_frames
        idx, p = idx[keep], p[keep]
        centre = first + len(p) / 2
        dist = np.abs(idx - centre)
        better = dist < best[idx]
        out[idx[better]] = p[better]
        best[idx[better]] = dist[better]
        if start + win >= len(wav):
            break
        start += hop
    return out


def segments(probs: np.ndarray, threshold: float, min_frames: int = 20):
    """Runs of frames above threshold, as in Respiro-en's BreathDetector."""
    above = np.flatnonzero(probs > threshold)
    if len(above) == 0:
        return []
    splits = np.split(above, np.flatnonzero(np.diff(above) != 1) + 1)
    return [(s[0] * HOP, s[-1] * HOP) for s in splits if len(s) > min_frames]


def overlaps(a, b) -> bool:
    """Same rule the app uses: intersection over half the shorter one."""
    inter = min(a[1], b[1]) - max(a[0], b[0])
    return inter > 0.5 * min(a[1] - a[0], b[1] - b[0])


def score(pred, truth, rejected):
    matched_truth = [t for t in truth if any(overlaps(p, t) for p in pred)]
    tp = sum(1 for p in pred if any(overlaps(p, t) for t in truth))
    fp = len(pred) - tp
    fp_on_rejected = sum(1 for p in pred
                         if not any(overlaps(p, t) for t in truth)
                         and any(overlaps(p, r) for r in rejected))
    starts, ends = [], []
    for t in truth:
        best = [p for p in pred if overlaps(p, t)]
        if best:
            p = min(best, key=lambda p: abs(p[0] - t[0]))
            starts.append(abs(p[0] - t[0]))
            ends.append(abs(p[1] - t[1]))
    prec = tp / len(pred) if pred else 0.0
    rec = len(matched_truth) / len(truth) if truth else 0.0
    f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
    return dict(pred=len(pred), tp=tp, fp=fp, fp_rej=fp_on_rejected,
                found=len(matched_truth), truth=len(truth), p=prec, r=rec, f1=f1,
                start_err=np.median(starts) * 1000 if starts else float("nan"),
                end_err=np.median(ends) * 1000 if ends else float("nan"))


def fmt(name, s):
    return (f"  {name:<28} P {s['p']:5.0%}  R {s['r']:5.0%}  F1 {s['f1']:4.2f}"
            f"  | wykryte {s['pred']:3}  trafne {s['tp']:3}  fałszywe {s['fp']:3}"
            f" (w tym na odrzuconych {s['fp_rej']:2})  znalezione {s['found']}/{s['truth']}"
            f"  | błąd granic: start {s['start_err']:4.0f} ms, koniec {s['end_err']:4.0f} ms")


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    media_dir = Path(sys.argv[1])
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    model = load_model(device)
    thresholds = [0.064, 0.2, 0.5, 0.8]
    modes = {"cały plik": None, "okna 10 s": 10.0, "okna 30 s": 30.0}
    totals = {}
    for ref_path in sorted(media_dir.glob("*.oddech.reference.json")):
        media = ref_path.with_name(ref_path.name.removesuffix(".oddech.reference.json"))
        ref = json.loads(ref_path.read_text())
        threshold = ref.get("threshold", 0.35)
        for tid, t in ref["tracks"].items():
            if not t.get("fix"):
                continue
            regs = [r for r in ref["regions"] if str(r["track"]) == tid]
            truth = [(r["start"], r["end"]) for r in regs if r["kind"] == "breath"]
            rejected = [(r["start"], r["end"]) for r in regs if r["kind"] == "not_breath"]
            heur = [(r["start"], r["end"]) for r in regs
                    if r.get("confidence") is not None and r["confidence"] >= threshold]
            print(f"\n{media.name} · ścieżka {tid} · wdechów w referencji: {len(truth)}")
            results = {"heurystyka (obecna)": score(heur, truth, rejected)}
            wav = load_audio(media, int(tid))
            for mode, chunk in modes.items():
                probs = frame_probs(model, wav, device, chunk)
                for th in thresholds:
                    results[f"Respiro {mode}, próg {th}"] = score(segments(probs, th), truth, rejected)
            for name, s in results.items():
                print(fmt(name, s))
                acc = totals.setdefault(name, dict(pred=0, tp=0, found=0, truth=0))
                for k in acc:
                    acc[k] += s[k]
    print("\nRAZEM")
    for name, a in totals.items():
        p = a["tp"] / a["pred"] if a["pred"] else 0.0
        r = a["found"] / a["truth"] if a["truth"] else 0.0
        f1 = 2 * p * r / (p + r) if p + r else 0.0
        print(f"  {name:<28} P {p:5.0%}  R {r:5.0%}  F1 {f1:4.2f}")


if __name__ == "__main__":
    main()
