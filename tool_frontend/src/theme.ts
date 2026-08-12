/**
 * One theme for every page — the whole point of folding six hand-styled apps
 * into one web app.
 *
 * MUI's defaults do most of the work; the deviations here are deliberate:
 *
 * - A warm amber primary, because ciabatta's identity is bread, and it reads
 *   clearly against both light and dark surfaces.
 * - A monospace stack promoted to a real typography variant: five of the six
 *   pages are showing logs, byte dumps, file paths, or step output, so this is
 *   a primary typeface here, not an accent.
 * - Denser defaults (smaller table padding, no uppercase buttons) because these
 *   are operator tools showing a lot of rows at once.
 */

import { createTheme, type Theme } from "@mui/material/styles";

export const monoFontStack = [
  "ui-monospace",
  "SFMono-Regular",
  "Menlo",
  "Consolas",
  "'Liberation Mono'",
  "monospace",
].join(", ");

export function buildTheme(mode: "light" | "dark"): Theme {
  const dark = mode === "dark";

  return createTheme({
    palette: {
      mode,
      primary: {
        main: dark ? "#e0a458" : "#a1662f",
      },
      secondary: {
        main: dark ? "#7fa5b8" : "#39616f",
      },
      background: dark
        ? { default: "#16130f", paper: "#211c17" }
        : { default: "#faf7f2", paper: "#ffffff" },
      success: { main: dark ? "#6fbf73" : "#2e7d32" },
      warning: { main: dark ? "#e0b458" : "#a16d1f" },
      error: { main: dark ? "#e57373" : "#c62828" },
    },
    shape: { borderRadius: 8 },
    typography: {
      fontFamily: [
        "ui-sans-serif",
        "system-ui",
        "-apple-system",
        "'Segoe UI'",
        "Roboto",
        "sans-serif",
      ].join(", "),
      h1: { fontSize: "1.75rem", fontWeight: 600 },
      h2: { fontSize: "1.375rem", fontWeight: 600 },
      h3: { fontSize: "1.125rem", fontWeight: 600 },
      button: { textTransform: "none", fontWeight: 500 },
    },
    components: {
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
        },
      },
      MuiCard: {
        defaultProps: { variant: "outlined" },
      },
      MuiTooltip: {
        defaultProps: { arrow: true },
      },
    },
  });
}
