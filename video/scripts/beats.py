"""Finds the beat grid of public/music.mp3 and writes src/beats.json.

The showcase cuts on these beats. Decoding uses Remotion's bundled ffmpeg:
    npx remotion ffmpeg -y -i public/music.mp3 -ac 1 -ar 22050 out/music.wav
    python scripts/beats.py
"""

import json
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
SR = 22050
HOP = 256

with wave.open(str(ROOT / "out" / "music.wav")) as w:
    x = np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").astype(np.float32) / 32768
seconds = len(x) / SR

# Onset strength: positive change of log energy in short frames.
frames = len(x) // HOP
energy = np.array([np.sum(x[i * HOP:(i + 1) * HOP] ** 2) for i in range(frames)])
loud = np.log1p(energy * 1000)
onset = np.maximum(0, np.diff(loud, prepend=loud[0]))
fr = SR / HOP  # onset frames per second

# Tempo: the autocorrelation peak between 90 and 160 BPM.
ac = np.correlate(onset - onset.mean(), onset - onset.mean(), mode="full")[len(onset) - 1:]
lags = np.arange(len(ac)) / fr
ok = (lags > 60 / 160) & (lags < 60 / 90)
period = lags[ok][np.argmax(ac[ok])]
bpm = 60 / period

# Phase: the offset whose beat grid collects the most onset strength.
best, phase = -1.0, 0.0
for p in np.arange(0, period, 1 / fr):
    idx = np.round((np.arange(p, seconds, period)) * fr).astype(int)
    idx = idx[idx < len(onset)]
    s = onset[idx].sum()
    if s > best:
        best, phase = s, p

# The drop: the strongest one-second rise in loudness after 2 s.
win = int(fr)
smooth = np.convolve(loud, np.ones(win) / win, mode="same")
rise = smooth[win:] - smooth[:-win]
start = int(2 * fr)
drop = (start + int(np.argmax(rise[start:int(10 * fr)]))) / fr + 0.5
# Snap the drop to the nearest beat.
k = round((drop - phase) / period)
drop = phase + k * period

out = {"bpm": round(bpm, 2), "period": round(period, 4), "phase": round(phase, 4), "drop": round(drop, 3), "seconds": round(seconds, 3)}
(ROOT / "src" / "beats.json").write_text(json.dumps(out, indent=2))
print(out)
