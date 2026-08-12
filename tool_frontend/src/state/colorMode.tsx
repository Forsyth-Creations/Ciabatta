/**
 * Light/dark mode, following the OS by default and overridable from the top bar.
 *
 * This lives in a context rather than local state because the toggle sits in
 * the AppBar, which is rendered by the router's root route — below the
 * ThemeProvider that consumes the value.
 */

import { createContext, useContext, useMemo, useState } from "react";
import type { ReactNode } from "react";
import useMediaQuery from "@mui/material/useMediaQuery";

const STORAGE_KEY = "ciabatta-color-mode";

type Mode = "light" | "dark";

interface ColorModeValue {
  mode: Mode;
  toggle: () => void;
}

const ColorModeContext = createContext<ColorModeValue | undefined>(undefined);

export function useColorMode(): ColorModeValue {
  const context = useContext(ColorModeContext);
  if (!context) throw new Error("useColorMode must be used inside a ColorModeProvider");
  return context;
}

export function ColorModeProvider({
  children,
}: {
  children: (mode: Mode) => ReactNode;
}) {
  const prefersDark = useMediaQuery("(prefers-color-scheme: dark)");
  const [override, setOverride] = useState<Mode | null>(
    () => (localStorage.getItem(STORAGE_KEY) as Mode | null) ?? null,
  );

  const mode: Mode = override ?? (prefersDark ? "dark" : "light");

  const value = useMemo<ColorModeValue>(
    () => ({
      mode,
      toggle: () => {
        const next: Mode = mode === "dark" ? "light" : "dark";
        localStorage.setItem(STORAGE_KEY, next);
        setOverride(next);
      },
    }),
    [mode],
  );

  return (
    <ColorModeContext.Provider value={value}>{children(mode)}</ColorModeContext.Provider>
  );
}
