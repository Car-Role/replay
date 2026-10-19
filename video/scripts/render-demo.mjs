// Renders the demo clips in public/demo/clips.json to out/demo/<id>.mp4.
// Pass clip ids to render only those: node scripts/render-demo.mjs drift-a
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";

const only = process.argv.slice(2);
const clips = JSON.parse(readFileSync("public/demo/clips.json", "utf8")).filter((c) => only.length === 0 || only.includes(c.id));
mkdirSync("out/demo", { recursive: true });
for (const c of clips) {
  const props = `out/demo/${c.id}.json`;
  writeFileSync(props, JSON.stringify(c));
  console.log(`rendering ${c.id} (${c.seconds} s)`);
  execFileSync("npx", ["remotion", "render", "src/index.ts", c.comp, `out/demo/${c.id}.mp4`, `--props=${props}`, "--crf=20", "--log=error"], {
    stdio: "inherit",
    shell: true,
  });
}
