# Replay

Instant replay for Windows games, in the style of NVIDIA ShadowPlay, written in Rust.
Replay keeps the last minutes of every running game in memory. Press a hotkey and it saves a clip.
It finds games by itself and can record several games at the same time.

> **Status:** Windows works today; build it from source (see below). macOS and Linux
> support is in progress and not usable yet. Ready-made downloads come with the first release.

## Features

- **Hardware encoding on the GPU.** Frames go from the game window to NVENC without passing through the CPU.
  AV1 (default), HEVC and H.264 are supported. NVIDIA is preferred; AMD and Intel encoders also work.
- **One buffer per game.** `Alt+F10` saves the game in front. `Alt+Shift+F10` saves all games.
- **Automatic game detection.** Detection uses game launchers (Steam, Epic, Jagex, Battle.net, Riot, EA,
  GOG, Ubisoft and others), game library folders, the Windows Game Bar game list, a list of known games,
  and fullscreen 3D apps. `Ctrl+Alt+F10` marks or unmarks the app in front as a game.
- **Separate audio tracks.** Game audio comes from that game's process only, so Discord and music are
  not recorded. The microphone has its own track. Track 1 is a mix of both, for players that only play
  one track.
- **Windowed games** are cropped to the game area (no title bar).
- **Library window** with thumbnails, playback, trim, join and audio remix. Video is never re-encoded.
- **Sharing.** Copy the file to the clipboard (paste it into Discord), upload to Streamable, copy the
  link, or upload every saved clip on its own.
- **Fast-start MP4.** Clips play in web players while they download.

## Resource use

Measured on a current 8-core desktop with an NVIDIA RTX GPU, release build, AV1 1080p60 at 25 Mbit/s:

| State | CPU | Memory |
|---|---|---|
| Idle (no game) | 0.0 ms over 30 s | 2.7 MB private |
| Recording one game, with game audio and mic | about 4 to 6% of one core (about 0.3% of the whole CPU) | about 115 MB, plus the buffer |
| Saving a 2-minute clip | about 0.3 s on a below-normal thread | the clip shares memory with the buffer |

The buffer holds compressed video only, so it needs about `bitrate × length`. For example, 25 Mbit/s for 120 s
is about 375 MB for each game. The GPU device is released when no game runs.

## Requirements

- Windows 10 version 2004 or later (Windows 11 recommended). Process-loopback audio needs 2004.
- A GPU with a hardware encoder. AV1 needs an RTX 40 series or newer.
- To build: Rust 1.88 or later and the MSVC build tools.

## Build

```bash
cargo build --release
```

The program is `target\release\replay.exe`.

If linking fails with `LNK1104: cannot open file 'msvcrt.lib'`, Rust found a Visual Studio install
without the C++ libraries. Build from a "x64 Native Tools" prompt, or run `vcvars64.bat` from a complete
install first.

Run the tests:

```bash
cargo test --release
```

## Use

1. Start `replay.exe`. A gray dot appears in the notification area. It turns red while a game records.
2. Play a game. Press `Alt+F10` to save the last 2 minutes.
3. Click the notification, or double-click the tray icon, to open the library.

**Hotkey conflict with the NVIDIA app:** the NVIDIA app uses `Alt+F10` for its own Instant Replay.
Turn Instant Replay off in the NVIDIA app (`Alt+Z` > Settings), or choose other keys in Replay's settings.
Replay takes the keys as soon as they are free; you do not need to restart it.

Clips go to `Videos\Replay\<Game>\`. Settings are in `%APPDATA%\Replay\config.toml`.
Logs are in `%LOCALAPPDATA%\Replay\`.

### Command line

| Command | What it does |
|---|---|
| `replay` | Starts the tray recorder. A second start opens the library. |
| `replay --library [clip]` | Opens the library, optionally with a clip selected. |
| `replay --settings` | Opens the library on the Settings tab. |
| `replay --save` / `--save-all` | Asks the running recorder to save (for Stream Deck or macro keys). |
| `replay --detect` | Lists open windows and shows which ones count as games, and why. |
| `replay --probe` | Shows the GPU, encoders and microphones. |
| `replay --selftest [s] [av1\|hevc\|h264]` | Records the main monitor, saves a clip and checks that it decodes. |
| `replay --verify <file>` | Reads a clip back through Media Foundation and reports each track. |
| `replay --trim <file> <start> <end>` | Saves a trimmed copy. |
| `replay --join <file> <file>...` | Joins clips that use the same video settings. |

## How it works

```
game window --WGC--> BGRA texture --D3D11 video processor--> NV12 texture --NVENC (MFT)--> packet ring
game process --WASAPI process loopback--> PCM --AAC--> packet ring
microphone   --WASAPI capture-----------> PCM --AAC--> packet ring
hotkey --> snapshot of the rings (no copy) --> MP4 writer (moov first) --> Videos\Replay\<Game>\
```

- `capture.rs`: Windows.Graphics.Capture. `MinUpdateInterval` limits frames to about the target rate.
- `gpu.rs`: D3D11 device on the NVIDIA adapter, and the GPU color converter and scaler.
- `encoder.rs`: the hardware encoder through Media Foundation in async mode. Keyframes come every 0.5 s.
- `audio.rs`: WASAPI capture, AAC encode and decode, and a timeline that fills gaps with silence.
- `ring.rs`: the packet ring. It drops whole GOPs, so every snapshot starts on a keyframe.
- `mux/`: Annex B and AV1 OBU handling, the MP4 writer and the MP4 reader.
- `detect.rs`: game detection. `app.rs`: the tray process. `session.rs`: one recording per game.
- `edit.rs`, `player.rs`, `ui/`: the library window. It is a separate process that exits when closed,
  so the recorder never loads UI code.

## Known limits

- NVIDIA does not offer a public API for ShadowPlay itself. Replay uses the same parts that ShadowPlay
  uses (desktop capture and NVENC), through public Windows APIs.
- A few games in true exclusive fullscreen give no window frames. Replay then records their monitor instead.
- HDR games are recorded in SDR.
- Trims and joins start on the keyframe at or before the chosen point (within 0.5 s), because video is
  not re-encoded. Joins need clips with the same codec, size and settings.
- The Streamable upload uses Streamable's public upload API with your email and password. It has not
  been tested against a live account yet.
