import React from "react";
import { AbsoluteFill, Audio, Easing, Img, interpolate, Sequence, spring, staticFile, useCurrentFrame, useVideoConfig } from "remotion";
import beats from "./beats.json";
import { NeonDrift } from "./games/NeonDrift";

// The app's own tokens (docs/design.md).
const C = {
  base: "#141414",
  text: "#FFFFFF",
  text2: "#C8C8C8",
  text3: "#9E9E9E",
  accent: "#F2649A",
  card: "#2B2B2B",
};
const FONT = "'Segoe UI Variable Display', 'Segoe UI', sans-serif";
export const FPS = 30;

/** Frame of beat `k` in the soundtrack (scripts/beats.py measured the grid). */
export const beat = (k: number) => Math.round((beats.phase + k * beats.period) * FPS);
export const SHOWCASE_FRAMES = Math.round(32 * FPS);

// Scene starts, in beats: two bars (8 beats) each, the title on the drop.
const S = { title: 8, library: 16, editor: 24, vertical: 32, discord: 40, games: 48, end: 56 };

const Fade: React.FC<{ children: React.ReactNode; inFrames?: number }> = ({ children, inFrames = 7 }) => {
  const f = useCurrentFrame();
  const o = interpolate(f, [0, inFrames], [0, 1], { extrapolateRight: "clamp" });
  return <AbsoluteFill style={{ opacity: o }}>{children}</AbsoluteFill>;
};

const Backdrop: React.FC = () => (
  <AbsoluteFill
    style={{
      background: `radial-gradient(1100px 700px at 78% 18%, rgba(242,100,154,0.20), transparent 70%),
        radial-gradient(900px 700px at 10% 100%, rgba(120,90,255,0.12), transparent 70%), ${C.base}`,
    }}
  />
);

const Keycap: React.FC<{ label: string; delay: number }> = ({ label, delay }) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame: f - delay, fps, config: { damping: 14, stiffness: 220 } });
  const press = interpolate(f - delay, [8, 11, 15], [0, 6, 0], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  return (
    <div
      style={{
        minWidth: 120,
        height: 104,
        padding: "0 30px",
        borderRadius: 18,
        background: "linear-gradient(#fafafa, #dcdcdc)",
        boxShadow: `0 ${10 - press}px 0 #9a9a9a, 0 18px 40px rgba(0,0,0,0.5)`,
        transform: `translateY(${press}px) scale(${s})`,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        fontFamily: FONT,
        fontWeight: 600,
        fontSize: 48,
        color: "#1a1a1a",
      }}
    >
      {label}
    </div>
  );
};

/** Gameplay, the hotkey, and the in-game notice. */
const Opener: React.FC = () => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const keys = beat(4);
  const saved = beat(5);
  const toast = spring({ frame: f - saved, fps, config: { damping: 18, stiffness: 160 } });
  const line1 = interpolate(f, [6, 18], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const line2 = interpolate(f, [saved, saved + 10], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const flash = interpolate(f, [saved, saved + 3, saved + 12], [0, 0.35, 0], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  return (
    <AbsoluteFill>
      <NeonDrift id="opener" game="Neon Drift" seconds={8} seed={3} events={[21.2, 26.0]} audio="" from={19.4} quiet />
      <AbsoluteFill style={{ background: "#fff", opacity: flash }} />
      <AbsoluteFill style={{ background: "linear-gradient(transparent 55%, rgba(0,0,0,0.75))" }} />
      <div style={{ position: "absolute", left: 120, bottom: 110, fontFamily: FONT, color: C.text }}>
        <div style={{ fontSize: 40, color: C.text2, opacity: line1 }}>That play just now?</div>
        <div style={{ fontSize: 76, fontWeight: 600, opacity: line2, transform: `translateY(${(1 - line2) * 20}px)` }}>It's already saved.</div>
      </div>
      <div style={{ position: "absolute", right: 120, bottom: 110, display: "flex", gap: 20, alignItems: "center" }}>
        {f >= keys - 2 ? (
          <>
            <Keycap label="Alt" delay={keys} />
            <span style={{ color: C.text, fontSize: 48, fontFamily: FONT, opacity: 0.8 }}>+</span>
            <Keycap label="F10" delay={keys + 2} />
          </>
        ) : null}
      </div>
      {/* Replay's in-game notice */}
      <div
        style={{
          position: "absolute",
          top: 150,
          right: 64,
          width: 470,
          padding: "22px 26px",
          borderRadius: 14,
          background: "rgba(32,32,32,0.92)",
          border: "1px solid rgba(255,255,255,0.10)",
          boxShadow: "0 20px 50px rgba(0,0,0,0.5)",
          fontFamily: FONT,
          color: C.text,
          display: "flex",
          gap: 20,
          alignItems: "center",
          opacity: toast,
          transform: `translateX(${(1 - toast) * 80}px)`,
        }}
      >
        <Img src={staticFile("icon.png")} style={{ width: 56, height: 56 }} />
        <div>
          <div style={{ fontSize: 30, fontWeight: 600 }}>Replay saved</div>
          <div style={{ fontSize: 24, color: C.text2 }}>Neon Drift · the last 2:00</div>
        </div>
      </div>
    </AbsoluteFill>
  );
};

const Title: React.FC = () => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame: f, fps, config: { damping: 12, stiffness: 180 } });
  const sub = interpolate(f, [10, 22], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", fontFamily: FONT, color: C.text }}>
      <Backdrop />
      <div style={{ display: "flex", alignItems: "center", gap: 40, transform: `scale(${0.85 + 0.15 * s})`, opacity: s }}>
        <Img src={staticFile("icon.png")} style={{ width: 170, height: 170 }} />
        <div style={{ fontSize: 168, fontWeight: 600, letterSpacing: -4 }}>Replay</div>
      </div>
      <div style={{ marginTop: 34, fontSize: 46, color: C.text2, opacity: sub, transform: `translateY(${(1 - sub) * 16}px)` }}>
        Instant replay for Windows games
      </div>
    </AbsoluteFill>
  );
};

/** A screenshot in a Windows 11 window, rising in and pushing slowly towards `focus`. */
const Feature: React.FC<{ shot: string; eyebrow: string; headline: string; focus: [number, number]; zoom: number; length: number }> = ({ shot, eyebrow, headline, focus, zoom, length }) => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const rise = spring({ frame: f, fps, config: { damping: 20, stiffness: 120 } });
  const push = interpolate(f, [0, length], [1, zoom], { easing: Easing.inOut(Easing.cubic), extrapolateRight: "clamp" });
  const text = interpolate(f, [2, 12], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  const W = 1440;
  const H = (W * 1080) / 1920;
  return (
    <AbsoluteFill style={{ fontFamily: FONT, color: C.text, overflow: "hidden" }}>
      <Backdrop />
      <div
        style={{
          position: "absolute",
          left: (1920 - W) / 2,
          top: 285,
          width: W,
          transform: `translateY(${(1 - rise) * 120}px) scale(${push})`,
          transformOrigin: `${focus[0] * 100}% ${focus[1] * 100}%`,
          opacity: rise,
        }}
      >
        <div
          style={{
            borderRadius: 14,
            overflow: "hidden",
            border: "1px solid rgba(255,255,255,0.12)",
            boxShadow: "0 40px 120px rgba(0,0,0,0.65), 0 0 0 1px rgba(0,0,0,0.5)",
            background: "#202020",
          }}
        >
          <div style={{ height: 44, display: "flex", alignItems: "center", gap: 12, padding: "0 16px", background: "#202020", fontSize: 18, color: C.text2 }}>
            <Img src={staticFile("icon.png")} style={{ width: 20, height: 20 }} />
            Replay
            <div style={{ flex: 1 }} />
            <span style={{ letterSpacing: 34, color: C.text3, fontSize: 20 }}>–□✕</span>
          </div>
          <div style={{ width: W, height: H, overflow: "hidden" }}>
            <Img
              src={staticFile(`shots/${shot}.jpg`)}
              style={{ width: W, height: H, display: "block" }}
            />
          </div>
        </div>
      </div>
      <AbsoluteFill style={{ background: `linear-gradient(transparent 82%, ${C.base})` }} />
      {/* The headline stays on top: the window slides under a fade as it pushes in. */}
      <div style={{ position: "absolute", left: 0, right: 0, top: 0, height: 300, background: `linear-gradient(${C.base} 62%, transparent)`, opacity: 0.94 }} />
      <div style={{ position: "absolute", left: 210, top: 92, opacity: text, transform: `translateY(${(1 - text) * 14}px)` }}>
        <div style={{ fontSize: 30, fontWeight: 600, color: C.accent, marginBottom: 6 }}>{eyebrow}</div>
        <div style={{ fontSize: 66, fontWeight: 600, letterSpacing: -1 }}>{headline}</div>
      </div>
    </AbsoluteFill>
  );
};

const End: React.FC = () => {
  const f = useCurrentFrame();
  const { fps } = useVideoConfig();
  const s = spring({ frame: f, fps, config: { damping: 16, stiffness: 140 } });
  const pill = interpolate(f, [12, 24], [0, 1], { extrapolateLeft: "clamp", extrapolateRight: "clamp" });
  return (
    <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", fontFamily: FONT, color: C.text }}>
      <Backdrop />
      <div style={{ display: "flex", alignItems: "center", gap: 28, opacity: s, transform: `translateY(${(1 - s) * 30}px)` }}>
        <Img src={staticFile("icon.png")} style={{ width: 120, height: 120 }} />
        <div style={{ fontSize: 120, fontWeight: 600, letterSpacing: -3 }}>Replay</div>
      </div>
      <div style={{ position: "relative", marginTop: 26, fontSize: 40, color: C.text2, opacity: s }}>Free and open source, for Windows 10 and 11</div>
      <div
        style={{
          marginTop: 56,
          padding: "22px 48px",
          borderRadius: 999,
          background: C.accent,
          color: "#1A0B12",
          fontSize: 40,
          fontWeight: 600,
          opacity: pill,
          transform: `scale(${0.9 + 0.1 * pill})`,
        }}
      >
        car-role.github.io/replay
      </div>
    </AbsoluteFill>
  );
};

const scenes: { at: number; node: React.ReactNode }[] = [
  { at: S.library, node: <Feature shot="library" eyebrow="Library" headline="Every clip, by day and by game." focus={[0.3, 0.5]} zoom={1.07} length={beat(S.editor) - beat(S.library)} /> },
  { at: S.editor, node: <Feature shot="editor" eyebrow="Editor" headline="Trim to the exact frame." focus={[0.55, 0.9]} zoom={1.1} length={beat(S.vertical) - beat(S.editor)} /> },
  { at: S.vertical, node: <Feature shot="vertical" eyebrow="Vertical" headline="Frame it 9:16 for Shorts and TikTok." focus={[0.45, 0.55]} zoom={1.08} length={beat(S.discord) - beat(S.vertical)} /> },
  { at: S.discord, node: <Feature shot="discord" eyebrow="Share" headline="Fits Discord's 10 MB in one click." focus={[0.9, 0.7]} zoom={1.14} length={beat(S.games) - beat(S.discord)} /> },
  { at: S.games, node: <Feature shot="games" eyebrow="Games" headline="Finds your games. Each gets its own quality." focus={[0.4, 0.4]} zoom={1.06} length={beat(S.end) - beat(S.games)} /> },
];

export const Showcase: React.FC = () => (
  <AbsoluteFill style={{ background: C.base }}>
    <Audio src={staticFile("music.mp3")} />
    <Sequence durationInFrames={beat(S.title) + 8}>
      <Opener />
    </Sequence>
    <Sequence from={beat(S.title)} durationInFrames={beat(S.library) - beat(S.title) + 8}>
      <Fade inFrames={3}>
        <Title />
      </Fade>
    </Sequence>
    {scenes.map((s, i) => {
      const next = i + 1 < scenes.length ? scenes[i + 1].at : S.end;
      return (
        <Sequence key={s.at} from={beat(s.at)} durationInFrames={beat(next) - beat(s.at) + 8}>
          <Fade>{s.node}</Fade>
        </Sequence>
      );
    })}
    <Sequence from={beat(S.end)}>
      <Fade>
        <End />
      </Fade>
    </Sequence>
  </AbsoluteFill>
);
