import React, { useMemo } from "react";
import { AbsoluteFill, Audio, staticFile, useCurrentFrame, useVideoConfig } from "remotion";
import { ClipProps, HUD_FONT, ramp, rng } from "./common";

// A made-up synthwave racer: a road into a striped sun, rivals to overtake.
const W = 1920;
const H = 1080;
const HORIZON = 560;
const VX = W / 2;

const project = (depth: number) => HORIZON + (H - HORIZON) / depth;

export const NeonDrift: React.FC<ClipProps> = ({ seed, events, audio, from = 0, quiet = false }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const t = from + frame / fps;

  const mountains = useMemo(() => {
    const r = rng(seed);
    const pts: number[] = [];
    for (let x = -200; x <= W * 2 + 200; x += 80) pts.push(x, HORIZON - 40 - r() * 150);
    return pts;
  }, [seed]);

  const steer = Math.sin(t * 0.55) * 0.7 + Math.sin(t * 1.7) * 0.15;
  const carX = VX + steer * 260;
  const speed = 268 + Math.sin(t * 0.8) * 22 + Math.sin(t * 2.9) * 6;

  // The road bends with the steering: the vanishing point moves the other way.
  const vx = VX - steer * 120;
  const roadX = (depth: number, side: number) => vx + (VX - vx) / depth + (side * 700) / depth;

  const scroll = (t * 9) % 1;
  const rows = Array.from({ length: 26 }, (_, i) => 1 + i + 1 - scroll);
  const passed = events.filter((e) => e <= t).length;
  const lastEvent = [...events].reverse().find((e) => e <= t);
  const flash = lastEvent === undefined ? 0 : 1 - ramp(t, lastEvent, lastEvent + 1.2);
  const place = Math.max(1, 6 - passed);
  const lap = Math.min(3, 1 + Math.floor(t / 40));
  const lapT = t % 40;

  // Rivals: each one is overtaken at its event time.
  const rivals = events
    .map((e, i) => ({ e, lane: i % 2 === 0 ? -0.45 : 0.45, hue: [190, 300, 45, 140][i % 4] }))
    .filter(({ e }) => t > e - 3.2 && t < e + 0.8)
    .map(({ e, lane, hue }) => {
      const k = ramp(t, e - 3.2, e + 0.8); // far -> past the camera
      const depth = 24 * Math.pow(1 - k, 2.2) + 0.7;
      return { depth, lane, hue };
    });

  return (
    <AbsoluteFill style={{ background: "#0b0221", overflow: "hidden" }}>
      {audio ? <Audio src={staticFile(audio)} startFrom={Math.round(from * fps)} /> : null}
      <svg width={W} height={H} viewBox={`0 0 ${W} ${H}`}>
        <defs>
          <linearGradient id="sky" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="#090018" />
            <stop offset="0.55" stopColor="#3a0b5c" />
            <stop offset="1" stopColor="#ff2e88" />
          </linearGradient>
          <linearGradient id="sun" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="#ffe45c" />
            <stop offset="1" stopColor="#ff2e88" />
          </linearGradient>
          <linearGradient id="ground" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="#1a0433" />
            <stop offset="1" stopColor="#05010d" />
          </linearGradient>
          <filter id="glow" x="-50%" y="-50%" width="200%" height="200%">
            <feGaussianBlur stdDeviation="6" result="b" />
            <feMerge>
              <feMergeNode in="b" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
          <clipPath id="sunCut">
            {Array.from({ length: 9 }, (_, i) => {
              const y = 300 + i * 30;
              const gap = i < 3 ? 0 : (i - 2) * 3;
              return <rect key={i} x="0" y={y + gap} width={W} height={30 - gap} />;
            })}
          </clipPath>
        </defs>
        <rect width={W} height={HORIZON} fill="url(#sky)" />
        {/* Stars */}
        {useMemo(() => {
          const r = rng(seed + 99);
          return Array.from({ length: 90 }, (_, i) => (
            <circle key={i} cx={r() * W} cy={r() * (HORIZON - 200)} r={r() * 1.6 + 0.4} fill="#fff" opacity={0.3 + r() * 0.6} />
          ));
        }, [seed])}
        <circle cx={vx} cy={HORIZON - 70} r={210} fill="url(#sun)" clipPath="url(#sunCut)" filter="url(#glow)" />
        <polygon
          points={`-400,${HORIZON} ${mountains.map((v, i) => (i % 2 === 0 ? v - ((t * 14) % 160) - 200 + (vx - VX) * 0.3 : v)).join(" ")} ${W * 2 + 400},${HORIZON}`}
          fill="#14052b"
          stroke="#ff2e88"
          strokeWidth={2}
          strokeOpacity={0.6}
        />
        <rect y={HORIZON} width={W} height={H - HORIZON} fill="url(#ground)" />
        {/* Grid: rows rush towards the camera, columns meet at the vanishing point */}
        {rows.map((d, i) => (
          <line key={i} x1={0} x2={W} y1={project(d)} y2={project(d)} stroke="#b026ff" strokeOpacity={0.15 + 0.6 / d} strokeWidth={Math.max(1, 3 / d)} />
        ))}
        {Array.from({ length: 31 }, (_, i) => {
          const side = (i - 15) * 0.6;
          return <line key={i} x1={roadX(30, side)} y1={HORIZON} x2={roadX(0.62, side)} y2={H} stroke="#b026ff" strokeOpacity={0.35} strokeWidth={1.5} />;
        })}
        {/* Road */}
        <polygon points={`${roadX(30, -1)},${HORIZON} ${roadX(30, 1)},${HORIZON} ${roadX(0.62, 1)},${H} ${roadX(0.62, -1)},${H}`} fill="#0a0216" opacity={0.92} />
        <line x1={roadX(30, -1)} y1={HORIZON} x2={roadX(0.62, -1)} y2={H} stroke="#00f0ff" strokeWidth={4} filter="url(#glow)" />
        <line x1={roadX(30, 1)} y1={HORIZON} x2={roadX(0.62, 1)} y2={H} stroke="#00f0ff" strokeWidth={4} filter="url(#glow)" />
        {rows.filter((_, i) => i % 2 === 0).map((d, i) => (
          <line key={i} x1={roadX(d, 0)} y1={project(d)} x2={roadX(d + 0.5, 0)} y2={project(d + 0.5)} stroke="#ffe45c" strokeWidth={Math.max(2, 10 / d)} strokeOpacity={0.85} />
        ))}
        {/* Rivals */}
        {rivals.map((r, i) => {
          const s = 1 / r.depth;
          const x = roadX(r.depth, r.lane);
          const y = project(r.depth);
          return (
            <g key={i} transform={`translate(${x} ${y}) scale(${s * 1.25})`} filter="url(#glow)">
              <polygon points="-170,0 170,0 130,-70 -130,-70" fill="#120022" stroke={`hsl(${r.hue} 100% 60%)`} strokeWidth={6} />
              <rect x={-150} y={-30} width={60} height={14} fill={`hsl(${r.hue} 100% 60%)`} />
              <rect x={90} y={-30} width={60} height={14} fill={`hsl(${r.hue} 100% 60%)`} />
            </g>
          );
        })}
        {/* The player's car */}
        <g transform={`translate(${carX} ${H - 70}) rotate(${steer * 4})`} filter="url(#glow)">
          <ellipse cx={0} cy={22} rx={230} ry={22} fill="#ff2e88" opacity={0.25} />
          <polygon points="-220,0 220,0 180,-95 120,-150 -120,-150 -180,-95" fill="#1b0030" stroke="#ff2e88" strokeWidth={6} />
          <polygon points="-110,-140 110,-140 150,-95 -150,-95" fill="#2d0a4e" stroke="#00f0ff" strokeWidth={3} />
          <rect x={-200} y={-55} width={110} height={20} rx={4} fill="#ff1744" />
          <rect x={90} y={-55} width={110} height={20} rx={4} fill="#ff1744" />
        </g>
      </svg>

      {/* HUD */}
      <div style={{ position: "absolute", left: 48, top: 36, fontFamily: HUD_FONT, color: "#fff", textShadow: "0 0 12px #ff2e88" }}>
        <div style={{ fontSize: 28, letterSpacing: 4, opacity: 0.8 }}>LAP {lap}/3</div>
        <div style={{ fontSize: 44, fontWeight: 700 }}>
          {Math.floor(lapT / 60)}:{String(Math.floor(lapT % 60)).padStart(2, "0")}.{String(Math.floor((lapT % 1) * 1000)).padStart(3, "0")}
        </div>
      </div>
      <div style={{ position: "absolute", right: 56, top: 30, fontFamily: HUD_FONT, color: "#fff", fontSize: 96, fontWeight: 700, fontStyle: "italic", textShadow: "0 0 18px #00f0ff" }}>
        {place}
        <span style={{ fontSize: 40 }}>{["", "ST", "ND", "RD", "TH", "TH"][Math.min(place, 5)]}</span>
      </div>
      {quiet ? null : (
      <div style={{ position: "absolute", left: 64, bottom: 44, fontFamily: HUD_FONT, color: "#fff", textShadow: "0 0 14px #ff2e88" }}>
        <div style={{ fontSize: 110, fontWeight: 700, fontStyle: "italic", lineHeight: 1 }}>{Math.round(speed)}</div>
        <div style={{ fontSize: 26, letterSpacing: 6, opacity: 0.8 }}>KM/H</div>
      </div>
      )}
      {flash > 0 && !quiet ? (
        <div
          style={{
            position: "absolute",
            left: 0,
            right: 0,
            top: 240,
            textAlign: "center",
            fontFamily: HUD_FONT,
            fontSize: 84,
            fontWeight: 700,
            fontStyle: "italic",
            color: "#ffe45c",
            opacity: flash,
            transform: `scale(${1 + (1 - flash) * 0.25})`,
            textShadow: "0 0 24px #ff2e88",
          }}
        >
          OVERTAKE +500
        </div>
      ) : null}
    </AbsoluteFill>
  );
};
