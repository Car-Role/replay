// Shared bits for the made-up games used as demo footage.

export type ClipProps = {
  id: string;
  game: string;
  seconds: number;
  seed: number;
  events: number[];
  /** Path under public/, or "" for no sound. */
  audio: string;
  /** Start this many seconds into the clip (the showcase reuses footage). */
  from?: number;
  /** Hide the speedometer and banners (the showcase puts captions there). */
  quiet?: boolean;
};

/** A small seeded random generator (mulberry32), so every render is the same. */
export const rng = (seed: number) => {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
};

export const clamp = (v: number, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, v));

/** 0..1 progress of `t` through [a, b]. */
export const ramp = (t: number, a: number, b: number) => clamp((t - a) / (b - a));

export const HUD_FONT = "Bahnschrift, 'Segoe UI', sans-serif";
