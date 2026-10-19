import React from "react";
import { Composition } from "remotion";
import { ClipProps } from "./games/common";
import { NeonDrift } from "./games/NeonDrift";
import { Starfall } from "./games/Starfall";
import { FPS as SHOW_FPS, Showcase, SHOWCASE_FRAMES } from "./Showcase";

const FPS = 30;

// Defaults for the studio; demo renders pass each clip's props (render-demo.mjs).
const sample: ClipProps = { id: "sample", game: "Neon Drift", seconds: 20, seed: 3, events: [3, 9, 15], audio: "" };

const byLength = ({ props }: { props: ClipProps }) => ({ durationInFrames: Math.round(props.seconds * FPS) });

export const Root: React.FC = () => (
  <>
    <Composition id="NeonDrift" component={NeonDrift} width={1920} height={1080} fps={FPS} durationInFrames={600} defaultProps={sample} calculateMetadata={byLength} />
    <Composition id="Showcase" component={Showcase} width={1920} height={1080} fps={SHOW_FPS} durationInFrames={SHOWCASE_FRAMES} />
    <Composition id="Starfall" component={Starfall} width={1920} height={1080} fps={FPS} durationInFrames={600} defaultProps={{ ...sample, game: "Starfall Arena", seed: 7 }} calculateMetadata={byLength} />
  </>
);
