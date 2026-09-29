"""Opens Respiro-en detections in the app without touching existing work.

For every `<media>.oddech.reference.json` in the media folder, creates
`<media_dir>/respiro/<media>` (a symlink to the original) with its own
project file holding Respiro-en detections as unreviewed model regions.
The original recording, its project and its reference are not modified.

Segments are cut at the model's validation threshold (0.064); each carries
the peak frame probability as its confidence, so the app's sensitivity
slider works on them.

Recordings without a reference get the first audio track fixed and the
others muted.

Usage: python respiro_preview.py [--force] <media_dir | media ...>
       (a directory means every recording in it that has a reference)
"""

import json
import sys
from pathlib import Path

import numpy as np
import torch

import eval_respiro as e

SEGMENT_THRESHOLD = 0.064
PROJECT_THRESHOLD = 0.5
# Windows of 3 minutes: each normalizes its own level (the speaker moves
# relative to the microphone over a long recording).
WINDOW_S = 180.0
FORCE = "--force" in sys.argv


def default_tracks(media: Path) -> dict:
    duck = {"gain_db": -18.0, "fade_ms": 15.0}
    return {str(i): {"fix": n == 0, "mute": n != 0, "solo": False, "duck": duck, "detected": False}
            for n, i in enumerate(e.audio_streams(media))}


def main():
    args = [a for a in sys.argv[1:] if a != "--force"]
    if not args:
        sys.exit(__doc__)
    if len(args) == 1 and Path(args[0]).is_dir():
        refs = sorted(Path(args[0]).resolve().glob("*.oddech.reference.json"))
        medias = [r.with_name(r.name.removesuffix(".oddech.reference.json")) for r in refs]
    else:
        medias = [Path(a).resolve() for a in args]
    device = torch.device("cuda" if torch.cuda.is_available() else "cpu")
    model = e.load_model(device)
    for media in medias:
        ref_path = media.with_name(media.name + ".oddech.reference.json")
        tracks = json.loads(ref_path.read_text())["tracks"] if ref_path.exists() else default_tracks(media)
        out_dir = media.parent / "respiro"
        out_dir.mkdir(exist_ok=True)
        link = out_dir / media.name
        proj_path = link.with_name(link.name + ".oddech.json")
        if proj_path.exists() and not FORCE:
            # It may hold the user's review of this preview.
            print(f"{proj_path}: już istnieje, pomijam (--force nadpisuje)")
            continue
        if not link.exists():
            link.symlink_to(media)
        project = {
            "version": 1,
            "media_file": media.name,
            # Same track setup; `detected` stops the app from running its own
            # detection over these regions.
            "tracks": {tid: {**t, "detected": True} if t.get("fix") else t for tid, t in tracks.items()},
            "regions": [],
            "threshold": PROJECT_THRESHOLD,
        }
        for tid, t in tracks.items():
            if not t.get("fix"):
                continue
            probs = e.frame_probs(model, e.load_audio(media, int(tid)), device, WINDOW_S)
            for start, end in e.segments(probs, SEGMENT_THRESHOLD):
                a, b = int(round(start / e.HOP)), int(round(end / e.HOP)) + 1
                project["regions"].append({
                    "id": len(project["regions"]) + 1,
                    "track": int(tid),
                    "start": start,
                    "end": end + e.HOP,
                    "kind": "breath",
                    "source": "model",
                    "confidence": float(np.max(probs[a:b])),
                })
        project["next_region_id"] = len(project["regions"]) + 1
        proj_path.write_text(json.dumps(project, indent=1))
        print(f"{proj_path}: {len(project['regions'])} wykryć")


if __name__ == "__main__":
    main()
