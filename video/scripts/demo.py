"""Demo data for the showcase: made-up games, so no real clips or names appear.

    python scripts/demo.py prepare           # public/demo/clips.json and one .wav per clip
    (render each clip with Remotion, see render-demo.mjs)
    python scripts/demo.py install <home>    # a Replay folder for REPLAY_HOME=<home>
"""

import json
import os
import random
import shutil
import sys
import time
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
DEMO = ROOT / "public" / "demo"
RATE = 48_000

# Newest first. "ago" is hours before now; the library groups by day.
CLIPS = [
    {"id": "drift-a", "game": "Neon Drift", "comp": "NeonDrift", "seconds": 96, "seed": 3, "ago": 0.6},
    {"id": "star-a", "game": "Starfall Arena", "comp": "Starfall", "seconds": 42, "seed": 7, "ago": 2.2},
    {"id": "star-b", "game": "Starfall Arena", "comp": "Starfall", "seconds": 28, "seed": 11, "ago": 21.0, "favorite": True, "tags": ["ace"]},
    {"id": "drift-b", "game": "Neon Drift", "comp": "NeonDrift", "seconds": 36, "seed": 5, "ago": 22.5, "tags": ["win"]},
    {"id": "drift-c", "game": "Neon Drift", "comp": "NeonDrift", "seconds": 51, "seed": 9, "ago": 26.0, "suffix": " (joined)"},
]

GAMES = [
    {"exe": "neondrift.exe", "name": "Neon Drift", "how": "Steam library", "recording": True},
    {"exe": "starfall.exe", "name": "Starfall Arena", "how": "Game folder", "recording": False},
    {"exe": "hollowpeak.exe", "name": "Hollow Peak", "how": "Steam library", "recording": False},
]


def events(clip):
    """Moments of action: overtakes in Neon Drift, kills in Starfall."""
    rng = random.Random(clip["seed"])
    gap = (5.0, 9.0) if clip["comp"] == "NeonDrift" else (2.6, 5.2)
    t, out = 3.0, []
    while t < clip["seconds"] - 2.0:
        out.append(round(t, 2))
        t += rng.uniform(*gap)
    return out


def env(n, attack, decay):
    a = max(1, int(attack * RATE))
    e = np.exp(-np.arange(n) / (decay * RATE))
    e[:a] *= np.linspace(0, 1, a)
    return e


def place(buf, at, sound, gain=1.0):
    i = int(at * RATE)
    if i >= len(buf):
        return
    j = min(len(buf), i + len(sound))
    buf[i:j] += sound[: j - i] * gain


def kick(rng):
    n = int(0.35 * RATE)
    t = np.arange(n) / RATE
    f = 40 + 90 * np.exp(-t * 30)
    return np.sin(2 * np.pi * np.cumsum(f) / RATE) * env(n, 0.002, 0.12)


def noise(rng, secs, decay, lowpass=0.0):
    n = int(secs * RATE)
    x = rng.standard_normal(n)
    if lowpass:
        # One-pole low-pass: darker bursts for explosions.
        y = np.empty(n)
        acc = 0.0
        for k in range(n):
            acc += lowpass * (x[k] - acc)
            y[k] = acc
        x = y / max(1e-9, np.abs(y).max())
    return x * env(n, 0.003, decay)


def sweep(f0, f1, secs, decay):
    n = int(secs * RATE)
    f = np.geomspace(f0, f1, n)
    return np.sign(np.sin(2 * np.pi * np.cumsum(f) / RATE)) * 0.5 * env(n, 0.001, decay)


def soundtrack(clip, evs):
    rng = np.random.default_rng(clip["seed"])
    n = int(clip["seconds"] * RATE)
    t = np.arange(n) / RATE
    out = np.zeros(n)
    if clip["comp"] == "NeonDrift":
        # Engine: a few harmonics of a slowly wandering pitch.
        f = 62 + 14 * np.sin(t * 0.7) + 6 * np.sin(t * 2.3)
        ph = 2 * np.pi * np.cumsum(f) / RATE
        out += 0.16 * (np.sin(ph) + 0.5 * np.sin(2 * ph) + 0.25 * np.sin(3 * ph))
        bpm = 118
        k = kick(rng)
        for b in np.arange(0, clip["seconds"], 60 / bpm):
            place(out, b, k, 0.35)
        for e in evs:  # overtake: a rising whoosh
            place(out, e - 0.45, sweep(200, 1400, 0.6, 0.25), 0.18)
            place(out, e - 0.4, noise(rng, 0.7, 0.18), 0.12)
    else:
        # A dark pad and a faster beat.
        out += 0.07 * (np.sin(2 * np.pi * 55 * t) + 0.6 * np.sin(2 * np.pi * 82.4 * t + np.sin(t * 0.5)))
        bpm = 140
        k = kick(rng)
        for b in np.arange(0, clip["seconds"], 60 / bpm):
            place(out, b, k, 0.3)
        boom = noise(rng, 1.0, 0.28, lowpass=0.05)
        for e in evs:
            for s in (0.32, 0.2, 0.08):  # three laser shots, then the explosion
                place(out, e - s, sweep(1900, 260, 0.14, 0.05), 0.16)
            place(out, e, boom, 0.6)
    out /= max(1e-9, np.abs(out).max()) / 0.85
    return out


def write_wav(path, mono):
    pcm = (np.clip(mono, -1, 1) * 32767).astype("<i2")
    stereo = np.repeat(pcm[:, None], 2, axis=1)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(stereo.tobytes())


def prepare():
    DEMO.mkdir(parents=True, exist_ok=True)
    out = []
    for c in CLIPS:
        evs = events(c)
        write_wav(DEMO / f"{c['id']}.wav", soundtrack(c, evs))
        out.append({**c, "events": evs, "audio": f"demo/{c['id']}.wav"})
    (DEMO / "clips.json").write_text(json.dumps(out, indent=2))
    print(f"wrote {len(out)} clips to {DEMO}")


def install(home):
    home = Path(home).resolve()
    if home.exists():
        shutil.rmtree(home)
    clips_dir, config, data = home / "clips", home / "config", home / "data"
    for d in (clips_dir, config, data):
        d.mkdir(parents=True)
    now = time.time()
    library = {}
    for c in CLIPS:
        src = ROOT / "out" / "demo" / f"{c['id']}.mp4"
        when = now - c["ago"] * 3600
        stamp = time.strftime("%Y-%m-%d %H-%M-%S", time.localtime(when))
        folder = clips_dir / c["game"]
        folder.mkdir(exist_ok=True)
        dst = folder / f"{c['game']} {stamp}{c.get('suffix', '')}.mp4"
        shutil.copyfile(src, dst)
        os.utime(dst, (when, when))
        meta = {}
        if c.get("favorite"):
            meta["favorite"] = True
        if c.get("tags"):
            meta["tags"] = c["tags"]
        if meta:
            library[str(dst)] = meta
    games = []
    for g in GAMES:
        games.append({**g, "path": "", "first_seen": int(now - 30 * 86400), "last_seen": int(now - 600), "capturing": False, "capture_started": 0})
    (data / "games.json").write_text(json.dumps({"games": games}, indent=2))
    (data / "library.json").write_text(json.dumps({"clips": library}, indent=2))
    out_dir = str(clips_dir).replace("\\", "\\\\")
    (config / "config.toml").write_text(
        f'output_dir = "{out_dir}"\n'
        "replay_seconds = 120\n\n"
        "[detection]\n"
        'exclude = ["hollowpeak.exe"]\n\n'
        '[games."starfall.exe"]\n'
        "fps = 144\n"
        "bitrate_kbps = 12000\n"
    )
    print(f"demo Replay folder ready: {home}")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "prepare":
        prepare()
    elif cmd == "install" and len(sys.argv) > 2:
        install(sys.argv[2])
    else:
        print(__doc__)
        sys.exit(2)
