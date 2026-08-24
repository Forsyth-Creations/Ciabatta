/**
 * One theme for every page — the whole point of folding six hand-styled apps
 * into one web app.
 *
 * The palette and the type are Forsyth Creations' own
 * (https://www.forsythcreations.com/branding), named here as the brand names
 * them so a value in this file can be checked against the brand sheet without
 * anybody having to match hex codes by eye. Two deliberate departures from a
 * stock MUI theme survive the rebrand, because they're about what these pages
 * *are* rather than about how they look:
 *
 * - A monospace stack promoted to a real typography variant: five of the six
 *   pages are showing logs, byte dumps, file paths, or step output, so this is
 *   a primary typeface here, not an accent.
 * - Denser defaults (smaller table padding, no uppercase buttons) because these
 *   are operator tools showing a lot of rows at once.
 *
 * Fonts are bundled rather than fetched from Google: the daemon serves this app
 * from inside the binary, frequently on a machine with no route to the
 * internet, and a brand typeface that silently falls back to Arial on exactly
 * the machines people use this on is not a brand typeface.
 */

import { createTheme, type Theme } from "@mui/material/styles";

// Barlow 300–600 for body text, Barlow Condensed 600/700 for headings — the
// weights the brand sheet specifies, and only those, so the bundle carries what
// is used and nothing else.
import "@fontsource/barlow/300.css";
import "@fontsource/barlow/400.css";
import "@fontsource/barlow/500.css";
import "@fontsource/barlow/600.css";
import "@fontsource/barlow-condensed/600.css";
import "@fontsource/barlow-condensed/700.css";

/**
 * The brand palette, by its own names.
 *
 * Exported because the graph canvas and the log viewer pick colours directly
 * rather than through MUI's semantic slots — a dependency edge and an ANSI
 * "green" are both brand decisions that `palette.primary` can't express.
 */
export const brand = {
  // Primary blues
  sky: "#6D9BCC",
  steel: "#4576A6",
  forsythBlue: "#204D71",
  navy: "#173953",
  midnight: "#0C2131",

  // Accents
  spring: "#7BC78C",
  pine: "#4A8D62",
  gold: "#F0C341",
  amber: "#D98F2D",
  coral: "#F17D6A",

  // Neutrals
  cloud: "#F4F4F4",
  mist: "#D1D1D1",
  gray: "#A0A0A0",
  slate: "#6A6A6A",
  charcoal: "#3A3A3A",
} as const;

/**
 * Coral, taken down to a weight that can carry an error message on paper.
 *
 * The brand's only red is Coral, which is pitched for a dark background and
 * fails WCAG AA as body text on Cloud. Rather than borrow a red from outside
 * the palette, this holds Coral's hue and drops its lightness until it passes —
 * so an error still reads as the brand's red, and still reads.
 */
const DEEP_CORAL = "#AE3D29";

export const monoFontStack = [
  "ui-monospace",
  "SFMono-Regular",
  "Menlo",
  "Consolas",
  "'Liberation Mono'",
  "monospace",
].join(", ");

const bodyFontStack = ["Barlow", "ui-sans-serif", "system-ui", "-apple-system", "sans-serif"].join(
  ", ",
);

const headingFontStack = ["'Barlow Condensed'", "Barlow", "ui-sans-serif", "system-ui"].join(", ");

/**
 * Headings are condensed, bold and uppercase, per the brand sheet.
 *
 * The letter-spacing isn't decoration: condensed uppercase set tight is hard to
 * read at small sizes, and these headings go down to 1.05rem.
 */
const heading = (fontSize: string) => ({
  fontFamily: headingFontStack,
  fontWeight: 700,
  textTransform: "uppercase" as const,
  letterSpacing: "0.04em",
  fontSize,
});

export function buildTheme(mode: "light" | "dark"): Theme {
  const dark = mode === "dark";

  return createTheme({
    palette: {
      mode,
      // Forsyth Blue is the identity colour, but it is nearly black against
      // Midnight — so dark mode steps up the same hue to Sky.
      primary: {
        main: dark ? brand.sky : brand.forsythBlue,
        dark: dark ? brand.steel : brand.navy,
        light: dark ? "#8FB4DA" : brand.steel,
        contrastText: dark ? brand.midnight : brand.cloud,
      },
      secondary: {
        main: dark ? brand.gold : brand.amber,
        contrastText: brand.midnight,
      },
      background: dark
        ? { default: brand.midnight, paper: brand.navy }
        : { default: brand.cloud, paper: "#FFFFFF" },
      text: dark
        ? { primary: brand.cloud, secondary: brand.mist }
        : { primary: brand.charcoal, secondary: brand.slate },
      divider: dark ? "rgba(109, 155, 204, 0.24)" : brand.mist,
      success: { main: dark ? brand.spring : brand.pine },
      warning: { main: dark ? brand.gold : brand.amber },
      error: { main: dark ? brand.coral : DEEP_CORAL },
      info: { main: dark ? brand.sky : brand.steel },
    },
    shape: { borderRadius: 8 },
    typography: {
      fontFamily: bodyFontStack,
      // Barlow's lighter weights are what the brand uses for running text; 300
      // is too fine for the small sizes these tools live at, so body settles at
      // 400 and the brand's range is used across the scale instead.
      fontWeightLight: 300,
      fontWeightRegular: 400,
      fontWeightMedium: 500,
      fontWeightBold: 600,
      h1: heading("1.85rem"),
      h2: heading("1.45rem"),
      h3: heading("1.15rem"),
      h4: heading("1.05rem"),
      subtitle1: { fontWeight: 600 },
      subtitle2: { fontWeight: 600 },
      // Sentence case for everything a person reads as a sentence — the brand's
      // rule, and the reason buttons aren't shouting.
      button: { textTransform: "none", fontWeight: 600, letterSpacing: "0.01em" },
    },
    components: {
      MuiCssBaseline: {
        styleOverrides: {
          // A tabular figure keeps counts, sizes and timings from jittering as
          // they tick up, which on these pages they constantly do.
          body: { fontVariantNumeric: "tabular-nums" },
        },
      },
      MuiAppBar: {
        defaultProps: { elevation: 0, color: "default" },
        styleOverrides: {
          root: ({ theme }) => ({
            borderBottom: `1px solid ${theme.palette.divider}`,
            backgroundImage: "none",
          }),
        },
      },
      MuiDrawer: {
        styleOverrides: {
          paper: ({ theme }) => ({
            borderRight: `1px solid ${theme.palette.divider}`,
            backgroundImage: "none",
          }),
        },
      },
      MuiTableCell: {
        styleOverrides: {
          root: { paddingTop: 6, paddingBottom: 6 },
          head: {
            fontFamily: headingFontStack,
            fontWeight: 700,
            textTransform: "uppercase",
            letterSpacing: "0.05em",
          },
        },
      },
      MuiCard: {
        defaultProps: { variant: "outlined" },
      },
      MuiTooltip: {
        defaultProps: { arrow: true },
      },
      MuiChip: {
        styleOverrides: {
          // Chips carry step names, workspace names and file globs; letting
          // them shout in condensed caps would make a graph of them unreadable.
          label: { fontWeight: 500 },
        },
      },
    },
  });
}
