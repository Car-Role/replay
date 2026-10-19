# Showcase video

The 32-second tour on the website, made with [Remotion](https://www.remotion.dev/).
The games in it (Neon Drift, Starfall Arena) are made up and drawn by Remotion too,
so no real clips or player names appear.

```bash
npm install

# 1. Demo clips: a clip list and sound effects, then render each clip
python scripts/demo.py prepare
node scripts/render-demo.mjs

# 2. App screenshots from a demo Replay folder (from the repository root)
python video/scripts/demo.py install "%TEMP%\replay-demo"
set REPLAY_HOME=%TEMP%\replay-demo
target\release\replay.exe --snapshot <dir> --scale 1.5
#    then copy 01-clips, 04-editor, 05-editor-vertical, 05b-editor-10mb-warning
#    and 07-game to public/shots/ as library, editor, vertical, discord, games

# 3. Music (ElevenLabs), then its beat grid; scenes cut on these beats
ELEVENLABS_KEY_FILE=<key file> node scripts/music.mjs
npx remotion ffmpeg -y -i public/music.mp3 -ac 1 -ar 22050 out/music.wav
python scripts/beats.py

# 4. Render
npm run showcase
npm run poster
```

The site uses `out/replay-showcase.mp4` and `out/poster.png` as `site/media/replay-showcase.mp4` and `site/media/poster.jpg`.
