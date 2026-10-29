/**
 * The chess page's artwork: an original set of flat, modern SVG pieces, and
 * the board colour themes a player can pick from.
 *
 * The pieces are drawn here rather than borrowed — the popular sets on chess
 * sites are their own artwork — and as SVG rather than font glyphs, so they
 * look the same on every machine and never get picked up as selectable text.
 * Each is a few bold shapes on a 100x100 grid: a soft vertical gradient fill,
 * a dark rounded outline, and a highlight stroke for depth.
 */

import { useState } from "react";

/** Piece shapes on a 100x100 grid, by FEN letter. Drawn as one filled group. */
const SHAPES: Record<string, string[]> = {
  P: [
    "M50 15a13 13 0 1 1 0 26a13 13 0 1 1 0-26z",
    "M37 44h26l-3 7H40z",
    "M41 51h18c1 10 5 18 11 27H30c6-9 10-17 11-27z",
    "M24 78h52a4 4 0 0 1 4 4v3a4 4 0 0 1-4 4H24a4 4 0 0 1-4-4v-3a4 4 0 0 1 4-4z",
  ],
  R: [
    "M27 15h11v8h6v-8h12v8h6v-8h11v20l-5 6H32l-5-6z",
    "M33 41h34l3 35H30z",
    "M24 76h52a4 4 0 0 1 4 4v5a4 4 0 0 1-4 4H24a4 4 0 0 1-4-4v-5a4 4 0 0 1 4-4z",
  ],
  N: [
    "M30 78c0-11 4-18 12-24l-4-3c-6 2-12 0-14-6c-2-6 5-12 11-18c5-6 10-10 14-12l3-8l6 8c11 6 17 20 17 36c0 10-2 18-2 27z",
    "M24 78h52a4 4 0 0 1 4 4v3a4 4 0 0 1-4 4H24a4 4 0 0 1-4-4v-3a4 4 0 0 1 4-4z",
  ],
  B: [
    "M50 5a5 5 0 1 1 0 10a5 5 0 1 1 0-10z",
    "M50 15c-11 8-17 18-17 27c0 8 7 12 17 12s17-4 17-12c0-9-6-19-17-27z",
    "M36 54h28a3 3 0 0 1 0 7H36a3 3 0 0 1 0-7z",
    "M41 61h18c2 7 6 12 10 17H31c4-5 8-10 10-17z",
    "M24 78h52a4 4 0 0 1 4 4v3a4 4 0 0 1-4 4H24a4 4 0 0 1-4-4v-3a4 4 0 0 1 4-4z",
  ],
  Q: [
    "M30 66L22 30l14 14l4-24l10 22l10-22l4 24l14-14l-8 36z",
    "M30 66h40l3 12H27z",
    "M24 78h52a4 4 0 0 1 4 4v3a4 4 0 0 1-4 4H24a4 4 0 0 1-4-4v-3a4 4 0 0 1 4-4z",
  ],
  K: [
    "M46 6h8v8h8v8h-8v12h-8V22h-8v-8h8z",
    "M31 64c-7-12-5-25 6-27c6-1 10 3 13 8c3-5 7-9 13-8c11 2 13 15 6 27z",
    "M30 64h40l3 14H27z",
    "M24 78h52a4 4 0 0 1 4 4v3a4 4 0 0 1-4 4H24a4 4 0 0 1-4-4v-3a4 4 0 0 1 4-4z",
  ],
};

/** Inner details drawn as lines over the fill, by FEN letter. */
const DETAILS: Record<string, string[]> = {
  N: ["M43 31a2.6 2.6 0 1 0 0.1 0z", "M60 24c6 8 8 18 8 30"],
  B: ["M55 26L46 38"],
  Q: [],
  K: ["M34 71h32"],
  R: ["M33 41h34"],
  P: [],
};

/** The queen's crown points, finished with a ball each. */
const QUEEN_BALLS: [number, number][] = [
  [22, 30],
  [40, 20],
  [60, 20],
  [78, 30],
];

const INK = { w: "#262626", b: "#0F0F0F" } as const;

/**
 * The shared gradients, rendered once per board. Every piece references them
 * by id, so a board of 32 pieces carries two gradients, not 32.
 */
export function PieceDefs() {
  return (
    <svg width="0" height="0" style={{ position: "absolute" }} aria-hidden>
      <defs>
        <linearGradient id="ciabatta-piece-w" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#FFFFFF" />
          <stop offset="1" stopColor="#D5D9DE" />
        </linearGradient>
        <linearGradient id="ciabatta-piece-b" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#5B6068" />
          <stop offset="1" stopColor="#24272C" />
        </linearGradient>
      </defs>
    </svg>
  );
}

/** One piece, from a code like `wN` or `bq`. */
export function Piece({ code, size }: { code: string; size: number }) {
  const side = code[0] === "w" ? "w" : "b";
  const letter = code[1]?.toUpperCase() ?? "P";
  const ink = INK[side];
  // Black pieces get a pale detail line so their features still read.
  const detail = side === "w" ? ink : "#A9AFB8";
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 100 100"
      aria-hidden
      style={{
        display: "block",
        pointerEvents: "none",
        filter: "drop-shadow(0 2px 1.5px rgba(0,0,0,0.35))",
        overflow: "visible",
      }}
    >
      <g
        fill={`url(#ciabatta-piece-${side})`}
        stroke={ink}
        strokeWidth={4}
        strokeLinejoin="round"
        strokeLinecap="round"
      >
        {SHAPES[letter].map((d) => (
          <path key={d} d={d} />
        ))}
        {letter === "Q" &&
          QUEEN_BALLS.map(([cx, cy]) => <circle key={cx} cx={cx} cy={cy} r={5} />)}
      </g>
      <g fill="none" stroke={detail} strokeWidth={3.2} strokeLinecap="round" strokeLinejoin="round">
        {DETAILS[letter].map((d) => (
          <path key={d} d={d} fill={letter === "N" && d.startsWith("M43") ? detail : "none"} />
        ))}
      </g>
    </svg>
  );
}

export interface BoardTheme {
  id: string;
  name: string;
  light: string;
  dark: string;
}

export const BOARD_THEMES: BoardTheme[] = [
  { id: "ocean", name: "Ocean", light: "#E4ECF4", dark: "#6D9BCC" },
  { id: "green", name: "Tournament", light: "#EEEED2", dark: "#769656" },
  { id: "wood", name: "Wood", light: "#F0D9B5", dark: "#B58863" },
  { id: "slate", name: "Slate", light: "#DEE3E6", dark: "#8CA2AD" },
  { id: "purple", name: "Lavender", light: "#EAE6F5", dark: "#8F7FBF" },
  { id: "coral", name: "Coral", light: "#F7E6DA", dark: "#D9836B" },
  { id: "night", name: "Night", light: "#9AA4B1", dark: "#3E4A5A" },
];

const THEME_KEY = "ciabatta-chess-board";

/**
 * The board theme this browser chose. A per-viewer preference, so it lives in
 * localStorage — which may be unavailable, so every access is guarded.
 */
export function useBoardTheme(): [BoardTheme, (id: string) => void] {
  const [id, setId] = useState(() => {
    try {
      return localStorage.getItem(THEME_KEY) ?? BOARD_THEMES[0].id;
    } catch {
      return BOARD_THEMES[0].id;
    }
  });
  const choose = (next: string) => {
    setId(next);
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch {
      // Not remembered, but still applied for this visit.
    }
  };
  return [BOARD_THEMES.find((t) => t.id === id) ?? BOARD_THEMES[0], choose];
}
