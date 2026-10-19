import React, { useMemo } from "react";
import { AbsoluteFill, Audio, staticFile, useCurrentFrame, useVideoConfig } from "remotion";
import { ClipProps, HUD_FONT, ramp, rng } from "./common";

// A made-up arena space shooter: raiders dive in and are shot down.
const W = 1920;
const H = 1080;

export const Starfall: React.FC<ClipProps> = ({ seed, events, audio, from = 0 }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const t = from + frame / fps;

  const stars = useMemo(() => {
    const r = rng(seed);
    return Array.from({ length: 260 }, () => ({ x: r() * W, y: r() * H, layer: 1 + Math.floor(r() * 3), tw: r() * 6 }));
  }, [seed]);

  const shipX = W / 2 + Math.sin(t * 0.9) * 420 + Math.sin(t * 2.3) * 60;
  const shipY = H - 170 + Math.sin(t * 1.4) * 14;

  const kills = events.filter((e) => e <= t).length;
  const score = kills * 250 + Math.floor(t * 7) * 10;
  const shield = 0.55 + 0.35 * Math.cos(t * 0.21);

  // Each raider dives in for 2.6 s, takes three shots, and explodes at its event.
  const raiders = events.map((e, i) => {
    const r = rng(seed * 31 + i);
    const x0 = 200 + r() * (W - 400);
    const k = ramp(t, e - 2.6, e);
    const x = x0 + Math.sin(k * Math.PI * 2 + i) * 120;
    const y = -80 + k * (260 + r() * 260);
    return { e, x, y, hue: [0, 280, 30][i % 3] };
  });

  return (
    <AbsoluteFill style={{ background: "#03020c", overflow: "hidden" }}>
      {audio ? <Audio src={staticFile(audio)} startFrom={Math.round(from * fps)} /> : null}
      <div
        style={{
          position: "absolute",
          inset: -200,
          background: `radial-gradient(900px 600px at ${30 + Math.sin(t * 0.05) * 10}% 30%, rgba(122,36,190,0.55), transparent 70%),
            radial-gradient(800px 700px at ${75 + Math.cos(t * 0.04) * 8}% 65%, rgba(13,110,160,0.5), transparent 70%)`,
        }}
      />
      <svg width={W} height={H} viewBox={`0 0 ${W} ${H}`} style={{ position: "absolute" }}>
        <defs>
          <filter id="g" x="-100%" y="-100%" width="300%" height="300%">
            <feGaussianBlur stdDeviation="7" result="b" />
            <feMerge>
              <feMergeNode in="b" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        </defs>
        {stars.map((s, i) => {
          const y = (s.y + t * 60 * s.layer * s.layer) % H;
          return <rect key={i} x={s.x} y={y} width={s.layer} height={s.layer * (s.layer === 3 ? 6 : 1)} fill="#fff" opacity={0.25 + 0.25 * s.layer + 0.15 * Math.sin(t * 3 + s.tw)} />;
        })}
        {raiders.map(({ e, x, y, hue }, i) => {
          if (t < e - 2.6 || t > e + 0.9) return null;
          if (t >= e) {
            // Explosion: a ring and sparks.
            const k = ramp(t, e, e + 0.9);
            return (
              <g key={i} filter="url(#g)" opacity={1 - k}>
                <circle cx={x} cy={y} r={20 + k * 170} fill="none" stroke="#ffb347" strokeWidth={10 * (1 - k) + 2} />
                <circle cx={x} cy={y} r={60 * (1 - k)} fill="#fff3c4" />
                {Array.from({ length: 14 }, (_, j) => {
                  const a = (j / 14) * Math.PI * 2 + i;
                  return <circle key={j} cx={x + Math.cos(a) * k * 240} cy={y + Math.sin(a) * k * 240} r={5} fill="#ff6a3d" />;
                })}
              </g>
            );
          }
          return (
            <g key={i} transform={`translate(${x} ${y})`} filter="url(#g)">
              <polygon points="0,50 -60,-20 -20,-10 0,-40 20,-10 60,-20" fill="#1a0610" stroke={`hsl(${hue} 100% 62%)`} strokeWidth={5} />
            </g>
          );
        })}
        {/* Laser shots in the 0.35 s before each kill */}
        {raiders.flatMap(({ e, x, y }, i) =>
          [0.32, 0.2, 0.08].map((s, j) => {
            const k = ramp(t, e - s, e - s + 0.12);
            if (k <= 0 || k >= 1) return null;
            const yy = shipY - 80 - (shipY - 80 - y) * k;
            return <line key={`${i}-${j}`} x1={shipX + (x - shipX) * k} y1={yy + 50} x2={shipX + (x - shipX) * k} y2={yy - 30} stroke="#5cf2ff" strokeWidth={8} filter="url(#g)" />;
          }),
        )}
        {/* The player's ship */}
        <g transform={`translate(${shipX} ${shipY})`} filter="url(#g)">
          <polygon points={`-26,70 0,${110 + Math.sin(t * 40) * 18} 26,70`} fill="#ff9a3d" />
          <polygon points="0,-90 70,70 0,40 -70,70" fill="#0b1a2e" stroke="#5cf2ff" strokeWidth={6} />
          <polygon points="0,-40 18,10 -18,10" fill="#5cf2ff" />
        </g>
      </svg>

      <div style={{ position: "absolute", left: 48, top: 34, fontFamily: HUD_FONT, color: "#fff", textShadow: "0 0 12px #5cf2ff" }}>
        <div style={{ fontSize: 26, letterSpacing: 5, opacity: 0.75 }}>SCORE</div>
        <div style={{ fontSize: 64, fontWeight: 700 }}>{score.toLocaleString("en-US")}</div>
      </div>
      <div style={{ position: "absolute", right: 56, top: 40, fontFamily: HUD_FONT, color: "#fff", fontSize: 40, letterSpacing: 6, textShadow: "0 0 12px #b26bff" }}>
        WAVE {2 + Math.floor(kills / 4)}
      </div>
      <div style={{ position: "absolute", right: 56, top: 100, fontFamily: HUD_FONT, color: "#fff", fontSize: 26, textAlign: "right", lineHeight: 1.6 }}>
        {events
          .filter((e) => e <= t && t - e < 4)
          .slice(-3)
          .map((e) => (
            <div key={e} style={{ opacity: 1 - ramp(t, e + 3, e + 4) }}>
              Raider down <span style={{ color: "#ffb347" }}>+250</span>
            </div>
          ))}
      </div>
      <div style={{ position: "absolute", left: 48, bottom: 48, width: 420, fontFamily: HUD_FONT, color: "#fff" }}>
        <div style={{ fontSize: 24, letterSpacing: 5, opacity: 0.75, marginBottom: 10 }}>SHIELD</div>
        <div style={{ height: 16, borderRadius: 8, background: "rgba(255,255,255,0.15)" }}>
          <div style={{ width: `${shield * 100}%`, height: "100%", borderRadius: 8, background: "#5cf2ff", boxShadow: "0 0 16px #5cf2ff" }} />
        </div>
      </div>
    </AbsoluteFill>
  );
};
