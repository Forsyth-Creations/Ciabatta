import { useEffect, useMemo, useState } from "react";
import {
  Alert,
  AppBar,
  Box,
  Chip,
  Container,
  CssBaseline,
  IconButton,
  Tab,
  Tabs,
  ThemeProvider,
  ToggleButton,
  ToggleButtonGroup,
  Toolbar,
  Tooltip,
  Typography,
  useMediaQuery,
} from "@mui/material";
import BrightnessAutoIcon from "@mui/icons-material/BrightnessAuto";
import DarkModeIcon from "@mui/icons-material/DarkModeOutlined";
import LightModeIcon from "@mui/icons-material/LightModeOutlined";
import RefreshIcon from "@mui/icons-material/Refresh";

import { api, hasToken, type Health } from "./api";
import { buildTheme } from "./theme";
import { useLoad } from "./useLoad";
import { Builds } from "./views/Builds";
import { Login } from "./views/Login";
import { Overview } from "./views/Overview";
import { Releases } from "./views/Releases";
import { Users } from "./views/Users";

type View = "overview" | "builds" | "releases" | "users";

const VIEWS: { view: View; path: string; label: string }[] = [
  { view: "overview", path: "/", label: "Cache" },
  { view: "builds", path: "/builds", label: "Builds" },
  { view: "releases", path: "/releases", label: "Releases" },
  { view: "users", path: "/users", label: "Users" },
];

function viewFromPath(): View {
  const path = location.pathname.replace(/\/+$/, "") || "/";
  return VIEWS.find((v) => v.path !== "/" && path.endsWith(v.path))?.view ?? "overview";
}

// ─── Theme choice ───────────────────────────────────────────────────────────
// The one thing the page keeps in browser storage. Never a credential.
type ThemeChoice = "auto" | "light" | "dark";
const THEME_KEY = "ciabatta-cache-theme";

function savedTheme(): ThemeChoice {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    /* storage blocked: follow the OS */
  }
  return "auto";
}

function saveTheme(choice: ThemeChoice) {
  try {
    if (choice === "auto") localStorage.removeItem(THEME_KEY);
    else localStorage.setItem(THEME_KEY, choice);
  } catch {
    /* not remembered; still applied */
  }
}

export function App() {
  const prefersDark = useMediaQuery("(prefers-color-scheme: dark)");
  const [choice, setChoice] = useState<ThemeChoice>(savedTheme);
  const mode = choice === "auto" ? (prefersDark ? "dark" : "light") : choice;
  const theme = useMemo(() => buildTheme(mode), [mode]);

  const [view, setView] = useState<View>(viewFromPath);
  const [refreshKey, setRefreshKey] = useState(0);
  const [signedIn, setSignedIn] = useState(false);
  const [readAt, setReadAt] = useState<Date | null>(null);

  useEffect(() => {
    const onPop = () => setView(viewFromPath());
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const health = useLoad(() => api<Health>("/api/health"), refreshKey);
  const open = health.data && (health.data.auth === "open" || health.data.auth === "none");
  const ready = Boolean(open || (signedIn && hasToken()));

  useEffect(() => {
    if (health.data) setReadAt(new Date());
  }, [health.data, refreshKey]);

  const go = (next: View) => {
    const path = VIEWS.find((v) => v.view === next)!.path;
    if (location.pathname !== path) history.pushState(null, "", path);
    setView(next);
  };

  // The server's identity, compressed to a chip; the rest is on hover.
  const short = health.data
    ? `v${health.data.version} · ${health.data.auth} · ${location.protocol === "https:" ? "HTTPS" : "HTTP"}`
    : health.error
      ? "unreachable"
      : "…";
  const detail = health.data
    ? [
        `ciabatta ${health.data.version}`,
        `auth: ${health.data.auth}`,
        location.protocol === "https:" ? "HTTPS" : "HTTP",
        health.data.release?.version ? `serving ciabatta ${health.data.release.version}` : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : (health.error ?? "Loading…");

  // One row: who this is, where you are, and the two controls. Phones wrap the
  // tabs onto a line of their own rather than squeezing them.
  return (
    <ThemeProvider theme={theme}>
      <CssBaseline />
      <AppBar position="sticky">
        <Container maxWidth="xl">
          <Toolbar
            disableGutters
            sx={{ minHeight: { xs: 48 }, columnGap: 1.5, flexWrap: { xs: "wrap", md: "nowrap" } }}
          >
            <Box component="img" src="/favicon.svg" alt="" sx={{ width: 24, height: 24 }} />
            <Typography
              variant="h1"
              noWrap
              sx={{ fontSize: "1.1rem", lineHeight: 1, mr: { md: 2 }, flexShrink: 0 }}
            >
              ciabatta remote cache
            </Typography>
            {ready && (
              <Tabs
                value={view}
                onChange={(_, next: View) => go(next)}
                variant="scrollable"
                scrollButtons={false}
                sx={{
                  order: { xs: 10, md: 0 },
                  width: { xs: "100%", md: "auto" },
                  flex: { md: 1 },
                  minHeight: 48,
                  "& .MuiTab-root": { minHeight: 48, minWidth: 0, px: 1.5 },
                }}
              >
                {VIEWS.map((v) => (
                  <Tab key={v.view} value={v.view} label={v.label} />
                ))}
              </Tabs>
            )}
            <Box sx={{ flex: { xs: 1, md: ready ? 0 : 1 } }} />
            <Tooltip title={detail}>
              <Chip
                size="small"
                variant="outlined"
                label={short}
                color={health.error ? "error" : "default"}
                sx={{ display: { xs: "none", sm: "inline-flex" }, height: 24 }}
              />
            </Tooltip>
            <ToggleButtonGroup
              size="small"
              exclusive
              value={choice}
              aria-label="Theme"
              onChange={(_, next: ThemeChoice | null) => {
                if (!next) return;
                setChoice(next);
                saveTheme(next);
              }}
              sx={{ "& .MuiToggleButton-root": { p: 0.5 } }}
            >
              <ToggleButton value="auto" aria-label="Follow the system setting">
                <Tooltip title="Follow the system">
                  <BrightnessAutoIcon sx={{ fontSize: 18 }} />
                </Tooltip>
              </ToggleButton>
              <ToggleButton value="light" aria-label="Light">
                <Tooltip title="Light">
                  <LightModeIcon sx={{ fontSize: 18 }} />
                </Tooltip>
              </ToggleButton>
              <ToggleButton value="dark" aria-label="Dark">
                <Tooltip title="Dark">
                  <DarkModeIcon sx={{ fontSize: 18 }} />
                </Tooltip>
              </ToggleButton>
            </ToggleButtonGroup>
            <Tooltip
              title={
                readAt
                  ? `Read at ${readAt.toLocaleTimeString()} — click to re-read`
                  : "Re-read everything from the server"
              }
            >
              <IconButton
                size="small"
                onClick={() => setRefreshKey((k) => k + 1)}
                aria-label="Refresh"
              >
                <RefreshIcon fontSize="small" />
              </IconButton>
            </Tooltip>
          </Toolbar>
        </Container>
      </AppBar>

      <Container maxWidth="xl" sx={{ pb: 8 }}>
        {health.error && (
          <Alert severity="error" sx={{ mt: 3 }}>
            {health.error}
          </Alert>
        )}
        {health.data && !ready && (
          <Login
            onSignedIn={() => {
              setSignedIn(true);
              setRefreshKey((k) => k + 1);
            }}
          />
        )}
        {ready && view === "overview" && <Overview refreshKey={refreshKey} />}
        {ready && view === "builds" && <Builds refreshKey={refreshKey} />}
        {ready && view === "releases" && <Releases refreshKey={refreshKey} />}
        {ready && view === "users" && <Users refreshKey={refreshKey} />}
      </Container>
    </ThemeProvider>
  );
}
