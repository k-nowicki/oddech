"""Loudness of detections relative to the speaker's local speech level.

Frame level: RMS in dB of 25 ms windows every 10 ms (same grid as the model).
Local speech level: 98th percentile of frame levels within +-90 s. A span
without speech (98th percentile less than 20 dB above its 10th percentile)
borrows the level of the nearest span that has speech, so silence is never
judged against itself. Whether a span has speech is decided against the
loudest speech in the recording (a span more than 15 dB below it has none);
this only tells speech from silence, the level itself stays local.
"""

import numpy as np

SR = 16000
HOP = 160
WIN = 400
BLOCK = 100  # frames per 1 s block
RADIUS = 90  # s
MAX_BELOW_LOUDEST = 15.0  # dB


def frame_db(wav: np.ndarray) -> np.ndarray:
    n = len(wav) // HOP + 1
    padded = np.pad(wav, (WIN // 2, WIN // 2 + HOP))
    idx = np.arange(WIN)[None, :] + (np.arange(n) * HOP)[:, None]
    frames = padded[idx]
    return 10 * np.log10((frames ** 2).mean(1) + 1e-10)


def speech_level(db: np.ndarray) -> np.ndarray:
    """Per-frame local speech level (dB)."""
    blocks = len(db) // BLOCK + 1
    padded = np.pad(db, (0, blocks * BLOCK - len(db)), constant_values=-100)
    b = padded.reshape(blocks, BLOCK)
    hi = np.empty(blocks)
    for i in range(blocks):
        w = b[max(0, i - RADIUS):i + RADIUS + 1].ravel()
        hi[i] = np.percentile(w, 98)
    has = hi >= np.percentile(hi, 95) - MAX_BELOW_LOUDEST
    if has.any() and not has.all():
        # Borrow the typical level of the nearest stretch of speech (its
        # edges are diluted by the silence next to them).
        runs = np.split(np.arange(blocks), np.flatnonzero(np.diff(has.astype(int))) + 1)
        speech_runs = [(r[0], r[-1], np.median(hi[r])) for r in runs if has[r[0]]]
        for r in runs:
            if has[r[0]]:
                continue
            mid = (r[0] + r[-1]) / 2
            nearest = min(speech_runs, key=lambda sr: min(abs(sr[0] - mid), abs(sr[1] - mid)))
            hi[r] = nearest[2]
    return np.repeat(hi, BLOCK)[:len(db)]


def relative_level(db, speech, start, end) -> float:
    a, b = int(start * 100), int(end * 100) + 1
    return float(db[a:b].max() - speech[a:b].max())
