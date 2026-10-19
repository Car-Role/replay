// Generates the showcase soundtrack with the ElevenLabs Music API.
// The sections follow the video's scenes (see src/timeline.ts), so the drop
// lands on the title card and the track ends with the video.
//
//   ELEVENLABS_KEY_FILE=<path to a key file> node scripts/music.mjs
//
// The key is read from the file and sent only to api.elevenlabs.io; it is never printed.
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";

const keyFile = process.env.ELEVENLABS_KEY_FILE;
if (!keyFile) {
  console.error("Set ELEVENLABS_KEY_FILE to the file that holds the API key.");
  process.exit(2);
}
const key = readFileSync(keyFile, "utf8").trim();

const plan = {
  positive_global_styles: [
    "modern electronic product promo",
    "120 BPM",
    "four on the floor",
    "punchy kick",
    "bright synth plucks",
    "warm analog bass",
    "confident and sleek",
    "instrumental",
  ],
  negative_global_styles: ["vocals", "lyrics", "lo-fi", "acoustic", "orchestral", "slow"],
  sections: [
    {
      section_name: "Riser",
      positive_local_styles: ["pulsing synth arpeggio", "filter sweep opening up", "building tension", "riser into a drop"],
      negative_local_styles: ["full drums"],
      duration_ms: 4000,
      lines: [],
    },
    {
      section_name: "Drop",
      positive_local_styles: ["big drop on the first beat", "full drums", "wide synth chords", "energetic"],
      negative_local_styles: ["breakdown"],
      duration_ms: 4000,
      lines: [],
    },
    {
      section_name: "Groove",
      positive_local_styles: ["driving steady groove", "crisp hi-hats", "bright plucked melody", "clear accents every two bars"],
      negative_local_styles: ["tempo change", "silence"],
      duration_ms: 20000,
      lines: [],
    },
    {
      section_name: "Ending",
      positive_local_styles: ["final big chord on the first beat", "short reverb tail", "clean ending"],
      negative_local_styles: ["fade in", "new melody"],
      duration_ms: 4000,
      lines: [],
    },
  ],
};

const res = await fetch("https://api.elevenlabs.io/v1/music?output_format=mp3_44100_192", {
  method: "POST",
  headers: { "xi-api-key": key, "Content-Type": "application/json" },
  body: JSON.stringify({ composition_plan: plan, model_id: "music_v1" }),
});
if (!res.ok) {
  console.error(`ElevenLabs returned ${res.status}: ${(await res.text()).slice(0, 400)}`);
  process.exit(1);
}
mkdirSync("public", { recursive: true });
const buf = Buffer.from(await res.arrayBuffer());
writeFileSync("public/music.mp3", buf);
console.log(`wrote public/music.mp3 (${(buf.length / 1024).toFixed(0)} KB)`);
