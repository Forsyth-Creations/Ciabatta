/**
 * Render captured command output with its ANSI colours intact.
 *
 * Watch sessions ask their commands for colour (the daemon sets `FORCE_COLOR`
 * and friends, since a pipe would otherwise turn it off), so the lines arriving
 * over SSE carry SGR escapes. Printed raw they're worse than no colour at all —
 * an `ESC[31m…ESC[0m` wrapped around every word that mattered — so they're
 * parsed here into styled spans, and every other escape a terminal would act on
 * is dropped.
 *
 * Deliberately hand-rolled rather than pulling in ansi-to-html or xterm.js: this
 * is one regex and a switch over SGR codes, and the alternative is a dependency
 * baked into the Rust binary for the graph of a feature this small.
 */

import { useMemo } from "react";
import { Box, useTheme } from "@mui/material";

/** A colour as written by the escape, resolved against the theme at render. */
type AnsiColor =
  | { kind: "indexed"; index: number }
  | { kind: "rgb"; r: number; g: number; b: number };

interface SegmentStyle {
  fg: AnsiColor | null;
  bg: AnsiColor | null;
  bold: boolean;
  dim: boolean;
  italic: boolean;
  underline: boolean;
  strike: boolean;
  /** SGR 7: swap foreground and background, defaults included. */
  inverse: boolean;
  /** SGR 8: the text is there but must not be shown. */
  hidden: boolean;
}

export interface AnsiSegment {
  text: string;
  style: SegmentStyle;
}

const EMPTY: SegmentStyle = {
  fg: null,
  bg: null,
  bold: false,
  dim: false,
  italic: false,
  underline: false,
  strike: false,
  inverse: false,
  hidden: false,
};

/**
 * One escape sequence: a CSI (parameters, optional intermediates, a final byte),
 * an OSC string ended by BEL or ST, or any other two-character escape.
 *
 * Everything matched is removed from the text; only CSI `m` — SGR — changes how
 * the following text looks. Cursor moves and erases are dropped rather than
 * acted on: this is a scrollback view, not a terminal, and there is no cursor
 * for them to move.
 */
// eslint-disable-next-line no-control-regex
const ESCAPE = /\u001b(?:\[([0-9;:?]*)([ -/]*)([@-~])|\][^\u0007\u001b]*(?:\u0007|\u001b\\)|[@-Z\\-_])/g;

/** Split `input` into runs of text that share one style. */
export function parseAnsi(input: string): AnsiSegment[] {
  if (!input.includes("\u001b")) return [{ text: input, style: EMPTY }];

  const segments: AnsiSegment[] = [];
  let style = EMPTY;
  let cursor = 0;

  ESCAPE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = ESCAPE.exec(input)) !== null) {
    if (match.index > cursor) {
      segments.push({ text: input.slice(cursor, match.index), style });
    }
    cursor = match.index + match[0].length;

    const [, params, intermediates, final] = match;
    // Private-parameter forms (`\u001b[?25l`, hiding the cursor) share the `m`
    // space but never mean colour.
    if (final === "m" && !intermediates && !params?.includes("?")) {
      style = applySgr(style, params ?? "");
    }
  }

  if (cursor < input.length) {
    segments.push({ text: input.slice(cursor), style });
  }
  return segments.filter((segment) => segment.text.length > 0);
}

/** Strip every escape, leaving the text a person would read. */
export function stripAnsi(input: string): string {
  if (!input.includes("\u001b")) return input;
  return input.replace(ESCAPE, "");
}

/** Fold one SGR sequence's codes into the running style. */
function applySgr(style: SegmentStyle, params: string): SegmentStyle {
  // An empty parameter list means 0, and `4:3` (curly underline) is a colon
  // form whose sub-parameters only refine an attribute we render plainly.
  const codes = (params === "" ? "0" : params).split(";").map((p) => Number(p.split(":")[0]) || 0);

  let next = { ...style };
  for (let i = 0; i < codes.length; i++) {
    const code = codes[i];
    switch (code) {
      case 0:
        next = { ...EMPTY };
        break;
      case 1:
        next.bold = true;
        break;
      case 2:
        next.dim = true;
        break;
      case 3:
        next.italic = true;
        break;
      case 4:
        next.underline = true;
        break;
      case 7:
        next.inverse = true;
        break;
      case 8:
        next.hidden = true;
        break;
      case 9:
        next.strike = true;
        break;
      case 21:
      case 22:
        next.bold = false;
        next.dim = false;
        break;
      case 23:
        next.italic = false;
        break;
      case 24:
        next.underline = false;
        break;
      case 27:
        next.inverse = false;
        break;
      case 28:
        next.hidden = false;
        break;
      case 29:
        next.strike = false;
        break;
      case 39:
        next.fg = null;
        break;
      case 49:
        next.bg = null;
        break;
      // 38/48 introduce an extended colour that consumes the codes after it.
      case 38:
      case 48: {
        const extended = readExtendedColor(codes, i);
        if (extended) {
          if (code === 38) next.fg = extended.color;
          else next.bg = extended.color;
          i = extended.next;
        }
        break;
      }
      default:
        if (code >= 30 && code <= 37) next.fg = { kind: "indexed", index: code - 30 };
        else if (code >= 90 && code <= 97) next.fg = { kind: "indexed", index: code - 90 + 8 };
        else if (code >= 40 && code <= 47) next.bg = { kind: "indexed", index: code - 40 };
        else if (code >= 100 && code <= 107) next.bg = { kind: "indexed", index: code - 100 + 8 };
        break;
    }
  }
  return next;
}

/**
 * Read a `38`/`48` extended colour starting at `i`, returning it and the index
 * of the last code it consumed. Null if the sequence is truncated.
 */
function readExtendedColor(
  codes: number[],
  i: number,
): { color: AnsiColor; next: number } | null {
  const mode = codes[i + 1];
  if (mode === 5 && codes.length > i + 2) {
    return { color: { kind: "indexed", index: codes[i + 2] }, next: i + 2 };
  }
  if (mode === 2 && codes.length > i + 4) {
    return {
      color: { kind: "rgb", r: codes[i + 2], g: codes[i + 3], b: codes[i + 4] },
      next: i + 4,
    };
  }
  return null;
}

/**
 * The 16 named colours, per theme mode.
 *
 * A terminal's palette assumes its own background; ours is a warm off-white or a
 * warm near-black, and the raw xterm values fail against both — `bright white`
 * is invisible on paper, plain `blue` is unreadable on black. These are the
 * standard hues held to a legible contrast against each mode's surface.
 */
const PALETTE: Record<"light" | "dark", string[]> = {
  dark: [
    "#5c554c", // black
    "#e57373", // red
    "#7ec97e", // green
    "#e0b458", // yellow
    "#79a6d2", // blue
    "#c792ea", // magenta
    "#6fc3c9", // cyan
    "#d6cfc5", // white
    "#7d746a", // bright black
    "#ff8a80",
    "#a5e0a5",
    "#f2ce7b",
    "#a3c6e8",
    "#dcb0f7",
    "#8fe0e5",
    "#f5efe6",
  ],
  light: [
    "#3b342c", // black
    "#c62828", // red
    "#2e7d32", // green
    "#8a6100", // yellow
    "#1565c0", // blue
    "#7b1fa2", // magenta
    "#00696e", // cyan
    "#6b6259", // white — a mid grey, since true white is not text on paper
    "#8a8078", // bright black
    "#e53935",
    "#388e3c",
    "#a97400",
    "#1976d2",
    "#8e24aa",
    "#00838f",
    "#4a423a",
  ],
};

/** Resolve a parsed colour to CSS, using xterm's cube and greys above 15. */
function toCss(color: AnsiColor, mode: "light" | "dark"): string {
  if (color.kind === "rgb") return `rgb(${color.r}, ${color.g}, ${color.b})`;

  const { index } = color;
  if (index < 16) return PALETTE[mode][index] ?? PALETTE[mode][7];
  if (index < 232) {
    // The 6×6×6 cube: xterm's levels are 0 then 95 upwards in steps of 40.
    const n = index - 16;
    const level = (v: number) => (v === 0 ? 0 : 55 + v * 40);
    return `rgb(${level(Math.floor(n / 36) % 6)}, ${level(Math.floor(n / 6) % 6)}, ${level(n % 6)})`;
  }
  const grey = 8 + (index - 232) * 10;
  return `rgb(${grey}, ${grey}, ${grey})`;
}

/**
 * Output text with its colours applied.
 *
 * `fallbackColor` is what unstyled text uses — the caller's own colour for the
 * line (stderr is red here), which an escape then overrides.
 */
export function AnsiText({ text, fallbackColor }: { text: string; fallbackColor?: string }) {
  const theme = useTheme();
  const mode = theme.palette.mode;
  const segments = useMemo(() => parseAnsi(text), [text]);

  // The overwhelmingly common case: no escapes at all, so render the string and
  // leave the DOM as light as it was before any of this existed.
  if (segments.length === 1 && segments[0].style === EMPTY) return <>{text}</>;

  const defaultFg = fallbackColor ?? theme.palette.text.primary;
  const defaultBg = theme.palette.background.default;

  return (
    <>
      {segments.map((segment, index) => {
        const { style } = segment;
        let fg = style.fg ? toCss(style.fg, mode) : undefined;
        let bg = style.bg ? toCss(style.bg, mode) : undefined;
        if (style.inverse) {
          [fg, bg] = [bg ?? defaultBg, fg ?? defaultFg];
        }

        const decoration = [style.underline && "underline", style.strike && "line-through"]
          .filter(Boolean)
          .join(" ");

        return (
          <Box
            key={index}
            component="span"
            sx={{
              color: style.hidden ? "transparent" : fg,
              bgcolor: bg,
              fontWeight: style.bold ? 700 : undefined,
              fontStyle: style.italic ? "italic" : undefined,
              // Dim is a brightness reduction in a terminal; opacity is the
              // equivalent that works whatever colour it lands on.
              opacity: style.dim ? 0.65 : undefined,
              textDecoration: decoration || undefined,
            }}
          >
            {segment.text}
          </Box>
        );
      })}
    </>
  );
}
