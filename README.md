<div align="center">

<img src="assets/generated/png/icon_128.png" width="96" alt="">

# Replay

**Instant replay for Windows games. Save the play after it happens.**

[![Release](https://img.shields.io/github/v/release/Car-Role/replay?label=download)](https://github.com/Car-Role/replay/releases/latest)
[![CI](https://github.com/Car-Role/replay/actions/workflows/ci.yml/badge.svg)](https://github.com/Car-Role/replay/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

[Download](https://car-role.github.io/replay/) · [Releases](https://github.com/Car-Role/replay/releases) · [Report a problem](https://github.com/Car-Role/replay/issues)

</div>

Replay keeps the last minutes of every game in memory, encoded by your GPU. Press <kbd>Alt</kbd>+<kbd>F10</kbd> and the clip is saved. It finds games by itself, records several at once, and costs almost nothing while you play.

## Features

**Record**
- Hardware encoding (NVENC, AMD, Intel) in AV1, HEVC or H.264, up to 240 fps
- Automatic game detection: launchers, game folders, Game Bar list and fullscreen apps
- One buffer per game; save the game in front or all of them
- Full session recording that streams to disk
- Game audio only (no Discord or music), mic on its own track, plus a mix
- Per-game quality, webcam overlay, HDR to SDR conversion, in-game "saved" notice

**Edit and share**
- Trim with a scrubbing filmstrip, join, and remix game and mic volume
- Exact-frame cuts, exports that fit Discord (10, 50 or 500 MB), vertical 9:16 for Shorts
- Montages with music
- Favorites, tags, sorting and storage limits
- Copy to clipboard, or upload to Streamable, Catbox, Litterbox or any ShareX uploader

## Install

Get **Replay-Setup-windows-x64.exe** from the [download page](https://car-role.github.io/replay/). It needs no admin rights. Updates download in the background and are checked against a signature before they install.

Requires Windows 10 (2004) or 11 and a GPU with a hardware video encoder. AV1 needs an NVIDIA RTX 40, AMD RX 7000 or Intel Arc GPU, or newer.

## Hotkeys

| Keys | Action |
|---|---|
| <kbd>Alt</kbd>+<kbd>F10</kbd> | Save a replay of the game in front |
| <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>F10</kbd> | Save every game |
| <kbd>Alt</kbd>+<kbd>F9</kbd> | Start or stop a session recording |
| <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>F10</kbd> | Mark or unmark the app in front as a game |

The NVIDIA app also uses <kbd>Alt</kbd>+<kbd>F10</kbd>. Turn off its Instant Replay, or change the keys in Settings.

## Performance

RTX GPU, AV1 1080p60 at 25 Mbit/s, release build:

| | CPU | Memory |
|---|---|---|
| No game running | 0 | 3 MB |
| Recording, with game audio and mic | ~5% of one core | ~115 MB + buffer |
| Saving a 2-minute clip | 0.3 s | shared with the buffer |

The buffer holds compressed video only: 25 Mbit/s for 2 minutes is about 375 MB per game.

## Build

```bash
cargo build --release
cargo test
```

Needs Rust 1.88+ and the MSVC build tools. The program is `target\release\replay.exe`.

<details>
<summary>Command line</summary>

| Command | Action |
|---|---|
| `replay` | Start the recorder (a second start opens the library) |
| `replay --library [clip]` | Open the library |
| `replay --save` / `--save-all` / `--record` | Control the running recorder (Stream Deck, macros) |
| `replay --trim <file> <a> <b>` | Fast trim |
| `replay --join <files>... [--music <file>]` | Join clips |
| `replay --export <file> <a> <b> [--size MB] [--vertical 0..1] [--codec h264]` | Re-encode: exact cut, size target, vertical crop |
| `replay --detect` / `--probe` | Show detected games / GPU, encoders and devices |
| `replay --selftest [s] [--webcam] [--hdr]` | Record the screen and check the result |
| `replay --verify <file>` | Decode a clip and report each track |
| `replay --check-update` | Download and verify the newest release |

</details>

<details>
<summary>How it works</summary>

```
game window ─WGC─▶ texture ─D3D11 video processor─▶ NV12 ─NVENC─▶ packet ring
game process ─WASAPI loopback─▶ AAC ─▶ packet ring
microphone ─WASAPI─▶ AAC ─▶ packet ring
hotkey ─▶ snapshot of the rings (no copy) ─▶ MP4 writer ─▶ Videos\Replay\<Game>\
```

Frames never leave the GPU. The ring drops whole GOPs, so every snapshot starts on a keyframe. The library is a separate process, so the recorder never loads UI code. Settings live in `%APPDATA%\Replay\config.toml`, logs in `%LOCALAPPDATA%\Replay\`.

</details>

<details>
<summary>Known limits</summary>

- Some games in exclusive fullscreen give no window frames; Replay records their monitor instead, and may hide the in-game notice.
- Fast trims start on the keyframe before the cut (within 0.5 s); turn on **Exact cut** for frame accuracy.
- A session recording cut off by a crash cannot be played (its index is written at the end).
- HDR conversion and the upload services have not been tested on real HDR displays or live accounts yet.
- macOS and Linux versions are paused.

</details>

## License

MIT
