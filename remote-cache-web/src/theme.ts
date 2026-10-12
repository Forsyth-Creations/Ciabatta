/**
 * The admin app's theme: the same Forsyth Creations palette and type as the
 * daemon's web app (`tool_frontend/src/theme.ts`), trimmed to what a page of
 * numbers and tables uses.
 *
 * Fonts are bundled rather than fetched: a cache server is often a headless
 * box with no route to the internet, and the page has to look right there.
 */

import { createTheme, type Theme } from "@mui/material/styles";

import "@fontsource/barlow/400.css";
import "@fontsource/barlow/500.css";
import "@fontsource/barlow/600.css";
import "@fontsource/barlow-condensed/600.css";
import "@fontsource/barlow-condensed/700.css";

const brand = {
  sky: "#6D9BCC",
  steel: "#4576A6",
  forsythBlue: "#204D71",
  navy: "#173953",
  midnight: "#0C2131",
  spring: "#7BC78C",
  pine: "#4A8D62",
  gold: "#F0C341",
  amber: "#D98F2D",
  coral: "#F17D6A",
  cloud: "#F4F4F4",
  mist: "#D1D1D1",
  slate: "#6A6A6A",
  charcoal: "#3A3A3A",
} as const;

/**
 * The colours chart marks wear, one set per mode.
 *
 * Not the brand's own Pine/Amber/Steel: those read as grey at chart weight and
 * the green/orange pair collapses for protanopes. These keep the brand's hues
 * but were stepped until they pass a colour-vision-deficiency check against
 * each mode's card surface — hits and misses are told apart by lightness as
 * well as hue. Each mode is its own validated set, not an automatic flip.
 */
export interface ChartColors {
  hit: string;
  miss: string;
  upload: string;
  /** Build outcomes. */
  success: string;
  failed: string;
  other: string;
}

const chartLight: ChartColors = {
  hit: "#1E7F5C",
  miss: "#E3912F",
  upload: "#3366B8",
  success: "#1E7F5C",
  failed: "#C2412D",
  other: "#A0A0A0",
};

const chartDark: ChartColors = {
  hit: "#279874",
  miss: "#CC7F26",
  upload: "#5B7FD6",
  success: "#279874",
  failed: "#E0644F",
  other: "#6A6A6A",
};

export function chartColors(mode: "light" | "dark"): ChartColors {
  return mode === "dark" ? chartDark : chartLight;
}

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
      primary: {
        main: dark ? brand.sky : brand.forsythBlue,
        dark: dark ? brand.steel : brand.navy,
        light: dark ? "#8FB4DA" : brand.steel,
        contrastText: dark ? "#151617" : brand.cloud,
      },
      secondary: { main: dark ? brand.gold : brand.amber },
      background: dark
        ? { default: "#151617", paper: "#1D1F21" }
        : { default: brand.cloud, paper: "#FFFFFF" },
      text: dark
        ? { primary: brand.cloud, secondary: brand.mist }
        : { primary: brand.charcoal, secondary: brand.slate },
      divider: dark ? "rgba(255, 255, 255, 0.13)" : brand.mist,
      success: { main: dark ? brand.spring : brand.pine },
      warning: { main: dark ? brand.gold : brand.amber },
      error: { main: dark ? brand.coral : "#AE3D29" },
      info: { main: dark ? brand.sky : brand.steel },
    },
    shape: { borderRadius: 8 },
    typography: {
      fontFamily: bodyFontStack,
      h1: heading("1.85rem"),
      h2: heading("1.3rem"),
      h3: heading("1.05rem"),
      overline: { fontFamily: headingFontStack, fontWeight: 600, letterSpacing: "0.08em" },
      button: { textTransform: "none", fontWeight: 600 },
    },
    components: {
      MuiCssBaseline: {
        styleOverrides: { body: { fontVariantNumeric: "tabular-nums" } },
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
      MuiTableCell: {
        styleOverrides: {
          root: { paddingTop: 6, paddingBottom: 6 },
          head: {
            fontFamily: headingFontStack,
            fontWeight: 700,
            textTransform: "uppercase",
            letterSpacing: "0.05em",
            whiteSpace: "nowrap",
          },
        },
      },
      MuiCard: {
        defaultProps: { variant: "outlined" },
        styleOverrides: { root: { backgroundImage: "none" } },
      },
      MuiPaper: { styleOverrides: { root: { backgroundImage: "none" } } },
      MuiTooltip: { defaultProps: { arrow: true } },
      MuiChip: { styleOverrides: { label: { fontWeight: 500 } } },
    },
  });
}
